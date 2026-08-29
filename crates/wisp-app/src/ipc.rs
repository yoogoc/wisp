use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::Context;
use tokio::net::UnixStream;
use tracing::debug;
use wisp_protocol::{ClientMessage, ServerMessage, framed, receive_message, send_message};

#[derive(Debug)]
pub(crate) enum OverlayAction {
    Select { session_id: String, index: usize },
}

pub(crate) fn spawn_subscription(
    socket: PathBuf,
    sender: mpsc::Sender<ServerMessage>,
    reconnect_ms: u64,
) {
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build overlay IPC runtime");
        runtime.block_on(async move {
            loop {
                match subscribe_once(&socket, &sender).await {
                    Ok(()) => return,
                    Err(error) => {
                        debug!(%error, "overlay subscription reconnecting");
                        tokio::time::sleep(Duration::from_millis(reconnect_ms)).await;
                    }
                }
            }
        });
    });
}

pub(crate) fn spawn_interaction_worker(socket: PathBuf, receiver: mpsc::Receiver<OverlayAction>) {
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build overlay interaction runtime");
        while let Ok(action) = receiver.recv() {
            let message = match action {
                OverlayAction::Select { session_id, index } => {
                    ClientMessage::SelectCandidate { session_id, index }
                }
            };
            match runtime.block_on(overlay_request(&socket, message)) {
                Ok(ServerMessage::Error { message }) => {
                    debug!(%message, "overlay interaction was rejected");
                }
                Ok(_) => {}
                Err(error) => debug!(%error, "overlay interaction failed"),
            }
        }
    });
}

pub(crate) fn default_socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("WISP_SOCKET") {
        return path.into();
    }
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
        || std::env::temp_dir().join(format!("wisp-{}.sock", socket_identity())),
        |directory| PathBuf::from(directory).join("wisp.sock"),
    )
}

async fn overlay_request(socket: &Path, message: ClientMessage) -> anyhow::Result<ServerMessage> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect to daemon at {}", socket.display()))?;
    let mut connection = framed(stream);
    send_message(&mut connection, &message).await?;
    receive_message(&mut connection)
        .await?
        .context("daemon closed overlay interaction without a response")
}

async fn subscribe_once(socket: &Path, sender: &mpsc::Sender<ServerMessage>) -> anyhow::Result<()> {
    let stream = UnixStream::connect(socket).await?;
    let mut connection = framed(stream);
    send_message(&mut connection, &ClientMessage::SubscribeOverlay).await?;
    while let Some(message) = receive_message(&mut connection).await? {
        if sender.send(message).is_err() {
            return Ok(());
        }
    }
    Err(anyhow::anyhow!("daemon closed overlay subscription"))
}

fn socket_identity() -> String {
    std::env::var("USER")
        .unwrap_or_else(|_| "user".into())
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .collect()
}
