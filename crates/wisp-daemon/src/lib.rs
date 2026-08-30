use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, bail};
use directories::ProjectDirs;
use std::os::unix::fs::PermissionsExt;
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{Mutex, broadcast},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use wisp_ai::{AiCompletionRequest, ProviderRegistry};
use wisp_config::{AiRuntimeConfig, TerminalConfig, WispConfig};
use wisp_core::CompletionEngine;
use wisp_platform::TerminalCursorLocator;
use wisp_protocol::{
    AcceptTarget, ApplyEdit, BufferSnapshot, ClientMessage, NavigationDirection, RenderModel,
    ServerMessage, framed, receive_message, send_message,
};

struct SessionState {
    snapshot: BufferSnapshot,
    model: RenderModel,
    ai_cancellation: CancellationToken,
}

struct DaemonState {
    engine: CompletionEngine,
    providers: Arc<ProviderRegistry>,
    requests: Mutex<RequestTracker>,
    sessions: Mutex<HashMap<String, SessionState>>,
    overlay: broadcast::Sender<ServerMessage>,
    ai: AiRuntimeConfig,
    terminal: TerminalConfig,
}

#[derive(Default)]
struct RequestTracker {
    latest: HashMap<String, u64>,
    dismissed: HashMap<String, u64>,
}

impl RequestTracker {
    fn register(&mut self, session_id: &str, request_id: u64) -> bool {
        if self
            .dismissed
            .get(session_id)
            .is_some_and(|dismissed| *dismissed >= request_id)
        {
            return false;
        }
        match self.latest.get(session_id) {
            Some(latest) if *latest >= request_id => false,
            _ => {
                self.latest.insert(session_id.to_owned(), request_id);
                true
            }
        }
    }

    fn is_latest(&self, session_id: &str, request_id: u64) -> bool {
        self.latest.get(session_id) == Some(&request_id)
            && self
                .dismissed
                .get(session_id)
                .is_none_or(|dismissed| *dismissed < request_id)
    }

    fn dismiss(&mut self, session_id: &str, request_id: u64) {
        self.dismissed
            .entry(session_id.to_owned())
            .and_modify(|dismissed| *dismissed = (*dismissed).max(request_id))
            .or_insert(request_id);
    }
}

pub async fn run(
    socket: PathBuf,
    config: PathBuf,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let config = WispConfig::load(&config)?;
    let providers = ProviderRegistry::from_config(config.clone())
        .map_err(|error| anyhow::anyhow!("initialize AI providers: {error}"))?;
    let (overlay, _) = broadcast::channel(config.daemon.overlay_channel_capacity);
    let ranking_path = default_ranking_path();
    let state = Arc::new(DaemonState {
        engine: CompletionEngine::default()
            .with_max_candidates(config.completion.max_candidates)
            .with_recency_ranking(config.completion.recency, ranking_path)
            .with_generator_config(config.generator),
        providers: Arc::new(providers),
        requests: Mutex::new(RequestTracker::default()),
        sessions: Mutex::new(HashMap::new()),
        overlay,
        ai: config.ai,
        terminal: config.terminal,
    });

    prepare_socket(&socket)?;
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("bind daemon socket {}", socket.display()))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("protect daemon socket {}", socket.display()))?;
    info!(path = %socket.display(), "wisp daemon listening");

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accept IPC connection")?;
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, state).await {
                        debug!(%error, "IPC connection ended with an error");
                    }
                });
            }
            () = shutdown.cancelled() => {
                info!("shutting down");
                break;
            }
        }
    }
    if socket.exists() {
        std::fs::remove_file(&socket)
            .with_context(|| format!("remove daemon socket {}", socket.display()))?;
    }
    Ok(())
}

async fn handle_connection(stream: UnixStream, state: Arc<DaemonState>) -> anyhow::Result<()> {
    let mut connection = framed(stream);
    let Some(message) = receive_message::<_, ClientMessage>(&mut connection).await? else {
        return Ok(());
    };
    match message {
        ClientMessage::SubscribeOverlay => {
            let existing = {
                let sessions = state.sessions.lock().await;
                sessions
                    .values()
                    .map(|session| session.model.clone())
                    .collect::<Vec<_>>()
            };
            for model in existing {
                send_message(&mut connection, &ServerMessage::Render { model }).await?;
            }
            let mut receiver = state.overlay.subscribe();
            loop {
                match receiver.recv().await {
                    Ok(message) => send_message(&mut connection, &message).await?,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "overlay subscriber lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
        ClientMessage::Complete { snapshot } => {
            let (model, publish) = complete(snapshot, Arc::clone(&state)).await;
            let response = ServerMessage::Render {
                model: model.clone(),
            };
            if publish {
                let overlay_message = if model.candidates.is_empty() && model.ghost_text.is_none() {
                    ServerMessage::Hidden {
                        session_id: model.session_id,
                    }
                } else {
                    response.clone()
                };
                let _ = state.overlay.send(overlay_message);
            }
            send_message(&mut connection, &response).await?;
        }
        ClientMessage::Navigate {
            session_id,
            direction,
        } => {
            let message = navigate(&state, &session_id, direction).await;
            if matches!(message, ServerMessage::Render { .. }) {
                let _ = state.overlay.send(message.clone());
            }
            send_message(&mut connection, &message).await?;
        }
        ClientMessage::SelectCandidate { session_id, index } => {
            let message = select_candidate(&state, &session_id, index).await;
            send_message(&mut connection, &message).await?;
        }
        ClientMessage::Accept { session_id, target } => {
            let message = accept(&state, &session_id, target).await;
            if matches!(message, ServerMessage::ApplyEdit { .. }) {
                let hidden = ServerMessage::Hidden {
                    session_id: session_id.clone(),
                };
                let _ = state.overlay.send(hidden);
                state.sessions.lock().await.remove(&session_id);
            }
            send_message(&mut connection, &message).await?;
        }
        ClientMessage::Dismiss {
            session_id,
            request_id,
        } => {
            let mut requests = state.requests.lock().await;
            let request_id = request_id
                .or_else(|| requests.latest.get(&session_id).copied())
                .unwrap_or_default();
            requests.dismiss(&session_id, request_id);
            drop(requests);
            let session = state.sessions.lock().await.remove(&session_id);
            let had_visible_content = session
                .as_ref()
                .is_some_and(|session| model_has_content(&session.model));
            if let Some(session) = session {
                session.ai_cancellation.cancel();
            }
            let message = ServerMessage::Hidden { session_id };
            let _ = state.overlay.send(message.clone());
            if had_visible_content {
                send_message(&mut connection, &message).await?;
            } else {
                send_message(
                    &mut connection,
                    &ServerMessage::Error {
                        message: "no visible suggestions to dismiss".into(),
                    },
                )
                .await?;
            }
        }
        ClientMessage::Ping => send_message(&mut connection, &ServerMessage::Pong).await?,
    }
    Ok(())
}

fn model_has_content(model: &RenderModel) -> bool {
    !model.candidates.is_empty() || model.ghost_text.is_some()
}

async fn complete(snapshot: BufferSnapshot, state: Arc<DaemonState>) -> (RenderModel, bool) {
    if !register_latest_request(&state, &snapshot).await {
        return (current_or_empty_model(&state, &snapshot).await, false);
    }
    if let Some(session) = state.sessions.lock().await.get(&snapshot.session_id) {
        session.ai_cancellation.cancel();
    }

    let candidates = state.engine.complete(&snapshot).await;
    let locator_snapshot = snapshot.clone();
    let terminal_config = state.terminal.clone();
    let located = tokio::task::spawn_blocking(move || {
        TerminalCursorLocator::from_config(&terminal_config)
            .locate_with_context(&locator_snapshot)
            .ok()
    })
    .await
    .ok()
    .flatten();
    let cancellation = CancellationToken::new();
    let model = RenderModel {
        request_id: snapshot.request_id,
        session_id: snapshot.session_id.clone(),
        terminal_application_id: located
            .as_ref()
            .map(|located| located.application_id.clone()),
        anchor: located.map(|located| located.anchor),
        candidates,
        selected: 0,
        ghost_text: None,
    };
    {
        let requests = state.requests.lock().await;
        if !requests.is_latest(&snapshot.session_id, snapshot.request_id) {
            drop(requests);
            return (current_or_empty_model(&state, &snapshot).await, false);
        }
        let mut sessions = state.sessions.lock().await;
        if let Some(previous) = sessions.remove(&snapshot.session_id) {
            previous.ai_cancellation.cancel();
        }
        sessions.insert(
            snapshot.session_id.clone(),
            SessionState {
                snapshot: snapshot.clone(),
                model: model.clone(),
                ai_cancellation: cancellation.clone(),
            },
        );
    }

    if should_request_ai(&snapshot, &state.providers, state.ai.min_command_chars) {
        tokio::spawn(request_ai(snapshot, state, cancellation));
    }
    (model, true)
}

async fn register_latest_request(state: &DaemonState, snapshot: &BufferSnapshot) -> bool {
    state
        .requests
        .lock()
        .await
        .register(&snapshot.session_id, snapshot.request_id)
}

async fn current_or_empty_model(state: &DaemonState, snapshot: &BufferSnapshot) -> RenderModel {
    state
        .sessions
        .lock()
        .await
        .get(&snapshot.session_id)
        .map(|session| session.model.clone())
        .unwrap_or_else(|| RenderModel {
            request_id: snapshot.request_id,
            session_id: snapshot.session_id.clone(),
            terminal_application_id: None,
            anchor: None,
            candidates: Vec::new(),
            selected: 0,
            ghost_text: None,
        })
}

fn should_request_ai(
    snapshot: &BufferSnapshot,
    providers: &ProviderRegistry,
    min_command_chars: usize,
) -> bool {
    providers.is_enabled()
        && snapshot.cursor == snapshot.buffer.chars().count()
        && snapshot.buffer.trim().chars().count() >= min_command_chars
        && !looks_sensitive(&snapshot.buffer)
}

fn looks_sensitive(buffer: &str) -> bool {
    let lower = buffer.to_ascii_lowercase();
    ["password", "passwd", "token=", "secret=", "private_key"]
        .iter()
        .any(|needle| lower.contains(needle))
}

async fn request_ai(
    snapshot: BufferSnapshot,
    state: Arc<DaemonState>,
    cancellation: CancellationToken,
) {
    tokio::select! {
        () = cancellation.cancelled() => return,
        () = tokio::time::sleep(Duration::from_millis(state.ai.debounce_ms)) => {}
    }
    let prefix: String = snapshot.buffer.chars().take(snapshot.cursor).collect();
    let suffix: String = snapshot.buffer.chars().skip(snapshot.cursor).collect();
    let request = AiCompletionRequest {
        request_id: snapshot.request_id,
        prefix,
        suffix,
        shell: format!("{:?}", snapshot.shell).to_lowercase(),
        cwd: snapshot.cwd.to_string_lossy().into_owned(),
        recent_commands: Vec::new(),
        max_output_chars: state.ai.max_output_chars,
    };
    match state.providers.complete(request, cancellation).await {
        Ok(Some(completion)) if !completion.suffix.is_empty() => {
            let updated = {
                let mut sessions = state.sessions.lock().await;
                let Some(session) = sessions.get_mut(&snapshot.session_id) else {
                    return;
                };
                if session.snapshot.request_id != snapshot.request_id {
                    return;
                }
                session.model.ghost_text = Some(completion.suffix);
                session.model.clone()
            };
            let _ = state.overlay.send(ServerMessage::Render { model: updated });
        }
        Ok(_) => {}
        Err(error) => debug!(%error, "AI suggestion failed"),
    }
}

async fn navigate(
    state: &DaemonState,
    session_id: &str,
    direction: NavigationDirection,
) -> ServerMessage {
    let mut sessions = state.sessions.lock().await;
    let Some(session) = sessions.get_mut(session_id) else {
        return ServerMessage::Error {
            message: "no active completion session".into(),
        };
    };
    let len = session.model.candidates.len();
    if len == 0 {
        return ServerMessage::Error {
            message: "no candidates to navigate".into(),
        };
    }
    session.model.selected = match direction {
        NavigationDirection::Previous => (session.model.selected + len - 1) % len,
        NavigationDirection::Next => (session.model.selected + 1) % len,
    };
    ServerMessage::Render {
        model: session.model.clone(),
    }
}

async fn select_candidate(state: &DaemonState, session_id: &str, index: usize) -> ServerMessage {
    let mut sessions = state.sessions.lock().await;
    let Some(session) = sessions.get_mut(session_id) else {
        return ServerMessage::Error {
            message: "no active completion session".into(),
        };
    };
    if index >= session.model.candidates.len() {
        return ServerMessage::Error {
            message: "candidate index is out of range".into(),
        };
    }
    session.model.selected = index;
    ServerMessage::Render {
        model: session.model.clone(),
    }
}

async fn accept(state: &DaemonState, session_id: &str, target: AcceptTarget) -> ServerMessage {
    let sessions = state.sessions.lock().await;
    let Some(session) = sessions.get(session_id) else {
        return ServerMessage::Error {
            message: "no active completion session".into(),
        };
    };
    let (mut edit, continue_completion) = match target {
        AcceptTarget::Candidate => {
            let Some(candidate) = session.model.candidates.get(session.model.selected) else {
                return ServerMessage::Error {
                    message: "no candidate to accept".into(),
                };
            };
            if let Err(error) = state.engine.record_selection(&session.snapshot, candidate) {
                debug!(%error, "could not persist suggestion recency");
            }
            (
                replace_character_range(
                    &session.snapshot.buffer,
                    candidate.replace_start,
                    candidate.replace_end,
                    &candidate.insert_text,
                ),
                candidate.kind == wisp_protocol::CandidateKind::Directory,
            )
        }
        AcceptTarget::GhostText => {
            let Some(ghost) = session.model.ghost_text.as_deref() else {
                return ServerMessage::Error {
                    message: "no ghost text to accept".into(),
                };
            };
            (
                replace_character_range(
                    &session.snapshot.buffer,
                    session.snapshot.cursor,
                    session.snapshot.cursor,
                    ghost,
                ),
                false,
            )
        }
    };
    edit.continue_completion = continue_completion;
    ServerMessage::ApplyEdit { edit }
}

fn replace_character_range(buffer: &str, start: usize, end: usize, replacement: &str) -> ApplyEdit {
    let prefix: String = buffer.chars().take(start).collect();
    let suffix: String = buffer.chars().skip(end).collect();
    let cursor = prefix.chars().count() + replacement.chars().count();
    ApplyEdit {
        buffer: format!("{prefix}{replacement}{suffix}"),
        cursor,
        continue_completion: false,
    }
}

pub fn default_socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("WISP_SOCKET") {
        return path.into();
    }
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
        || std::env::temp_dir().join(format!("wisp-{}.sock", socket_identity())),
        |directory| PathBuf::from(directory).join("wisp.sock"),
    )
}

fn socket_identity() -> String {
    std::env::var("USER")
        .unwrap_or_else(|_| "user".into())
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .collect()
}

pub fn default_config_path() -> PathBuf {
    ProjectDirs::from("dev", "wisp", "wisp")
        .map(|dirs| dirs.config_dir().join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("wisp.toml"))
}

fn default_ranking_path() -> PathBuf {
    ProjectDirs::from("dev", "wisp", "wisp")
        .map(|dirs| dirs.data_local_dir().join("ranking.json"))
        .unwrap_or_else(|| PathBuf::from("ranking.json"))
}

fn prepare_socket(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket directory {}", parent.display()))?;
    }
    if path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            ensure_no_running_instance(path)?;
            if !path
                .symlink_metadata()
                .context("inspect existing socket path")?
                .file_type()
                .is_socket()
            {
                bail!("refusing to replace non-socket path {}", path.display());
            }
        }
        std::fs::remove_file(path)
            .with_context(|| format!("remove stale socket {}", path.display()))?;
    }
    Ok(())
}

pub fn ensure_no_running_instance(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    if path.exists() && std::os::unix::net::UnixStream::connect(path).is_ok() {
        bail!(
            "another Wisp instance is already listening at {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_uses_character_indices() {
        assert_eq!(
            replace_character_range("echo 你h", 5, 7, "你好"),
            ApplyEdit {
                buffer: "echo 你好".into(),
                cursor: 7,
                continue_completion: false,
            }
        );
    }

    #[test]
    fn sensitive_commands_are_not_sent_to_ai() {
        assert!(looks_sensitive("curl -H token=abc"));
        assert!(!looks_sensitive("cargo test"));
    }

    #[test]
    fn older_requests_cannot_replace_the_latest_sequence() {
        let mut requests = RequestTracker::default();
        assert!(requests.register("shell", 1));
        assert!(requests.register("shell", 3));
        assert!(!requests.register("shell", 2));
        assert!(!requests.register("shell", 3));
        assert!(requests.is_latest("shell", 3));
    }

    #[test]
    fn dismissed_request_cannot_reappear() {
        let mut requests = RequestTracker::default();
        assert!(requests.register("shell", 4));
        requests.dismiss("shell", 4);
        assert!(!requests.is_latest("shell", 4));
        assert!(!requests.register("shell", 4));
        assert!(requests.register("shell", 5));
        assert!(requests.is_latest("shell", 5));
    }

    #[cfg(unix)]
    #[test]
    fn active_daemon_socket_is_never_removed_as_stale() {
        let path = PathBuf::from(format!(
            "/tmp/wisp-active-socket-test-{}.sock",
            std::process::id()
        ));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();

        let error = prepare_socket(&path).unwrap_err();
        assert!(error.to_string().contains("already listening"));
        assert!(path.exists());

        drop(listener);
        std::fs::remove_file(path).unwrap();
    }
}
