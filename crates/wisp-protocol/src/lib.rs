use std::path::PathBuf;

use anyhow::Context;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub type SessionId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    Zsh,
    Bash,
    Fish,
    Nushell,
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKind {
    Alacritty,
    AppleTerminal,
    Iterm2,
    Ghostty,
    Wezterm,
    Kitty,
    Warp,
    Vscode,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalSnapshot {
    pub kind: TerminalKind,
    pub window_id: Option<String>,
    pub columns: u16,
    pub rows: u16,
    pub prompt: String,
    /// Zero-based terminal grid row reported by the terminal, when available.
    #[serde(default)]
    pub cursor_row: Option<u16>,
    /// Zero-based terminal grid column reported by the terminal, when available.
    #[serde(default)]
    pub cursor_column: Option<u16>,
    /// ZLE state currently painted on screen when `cursor_row` was sampled.
    #[serde(default)]
    pub rendered: Option<RenderedCursorSnapshot>,
    /// Pane content origin and the full terminal grid used by a multiplexer.
    #[serde(default)]
    pub viewport: Option<TerminalViewport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedCursorSnapshot {
    pub buffer: String,
    pub cursor: usize,
    pub prompt: String,
    pub columns: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalViewport {
    pub x: u16,
    pub y: u16,
    pub grid_columns: u16,
    pub grid_rows: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BufferSnapshot {
    pub request_id: u64,
    pub session_id: SessionId,
    pub buffer: String,
    /// Character index, matching ZLE's `$CURSOR` semantics.
    pub cursor: usize,
    pub cwd: PathBuf,
    pub shell: ShellKind,
    pub terminal: TerminalSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScreenPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CursorAnchor {
    pub position: ScreenPoint,
    pub line_height: f32,
    pub cell_width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Command,
    Subcommand,
    Option,
    File,
    Directory,
    Branch,
    History,
    Ai,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub label: String,
    pub insert_text: String,
    pub description: Option<String>,
    pub kind: CandidateKind,
    /// Fig-style ranking priority after recency has been applied.
    #[serde(default = "default_candidate_priority")]
    pub priority: f64,
    pub score: f64,
    /// Character range in the original shell buffer.
    pub replace_start: usize,
    pub replace_end: usize,
}

const fn default_candidate_priority() -> f64 {
    50.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderModel {
    pub request_id: u64,
    pub session_id: SessionId,
    /// Bundle identifier of the terminal application that produced this model.
    /// The overlay uses it to hide stale results as soon as another app is focused.
    #[serde(default)]
    pub terminal_application_id: Option<String>,
    pub anchor: Option<CursorAnchor>,
    pub candidates: Vec<Candidate>,
    pub selected: usize,
    pub ghost_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplyEdit {
    pub buffer: String,
    pub cursor: usize,
    pub continue_completion: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavigationDirection {
    Previous,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptTarget {
    Candidate,
    GhostText,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Complete {
        snapshot: BufferSnapshot,
    },
    Navigate {
        session_id: SessionId,
        direction: NavigationDirection,
    },
    SelectCandidate {
        session_id: SessionId,
        index: usize,
    },
    Accept {
        session_id: SessionId,
        target: AcceptTarget,
    },
    Dismiss {
        session_id: SessionId,
        request_id: Option<u64>,
    },
    SubscribeOverlay,
    Ping,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Render { model: RenderModel },
    ApplyEdit { edit: ApplyEdit },
    Hidden { session_id: SessionId },
    Pong,
    Error { message: String },
}

pub type MessageFramed<T> = Framed<T, LengthDelimitedCodec>;

pub fn framed<T>(io: T) -> MessageFramed<T>
where
    T: AsyncRead + AsyncWrite,
{
    Framed::new(io, LengthDelimitedCodec::new())
}

pub async fn send_message<T, M>(framed: &mut MessageFramed<T>, message: &M) -> anyhow::Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin,
    M: Serialize,
{
    let bytes = serde_json::to_vec(message).context("serialize IPC message")?;
    framed.send(bytes.into()).await.context("send IPC message")
}

pub async fn receive_message<T, M>(framed: &mut MessageFramed<T>) -> anyhow::Result<Option<M>>
where
    T: AsyncRead + AsyncWrite + Unpin,
    M: for<'de> Deserialize<'de>,
{
    let Some(frame) = framed.next().await.transpose().context("read IPC frame")? else {
        return Ok(None);
    };
    serde_json::from_slice(&frame)
        .context("decode IPC message")
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn protocol_round_trip_supports_multiline_buffers() {
        let (left, right) = tokio::io::duplex(4096);
        let mut sender = framed(left);
        let mut receiver = framed(right);
        let message = ClientMessage::Complete {
            snapshot: BufferSnapshot {
                request_id: 7,
                session_id: "test".into(),
                buffer: "echo one\necho two".into(),
                cursor: 17,
                cwd: PathBuf::from("/tmp"),
                shell: ShellKind::Zsh,
                terminal: TerminalSnapshot {
                    kind: TerminalKind::Alacritty,
                    window_id: Some("42".into()),
                    columns: 80,
                    rows: 24,
                    prompt: "$ ".into(),
                    cursor_row: Some(7),
                    cursor_column: Some(12),
                    rendered: Some(RenderedCursorSnapshot {
                        buffer: "echo one".into(),
                        cursor: 8,
                        prompt: "$ ".into(),
                        columns: 80,
                    }),
                    viewport: Some(TerminalViewport {
                        x: 1,
                        y: 2,
                        grid_columns: 160,
                        grid_rows: 48,
                    }),
                },
            },
        };

        send_message(&mut sender, &message).await.unwrap();
        let decoded: ClientMessage = receive_message(&mut receiver).await.unwrap().unwrap();
        assert_eq!(decoded, message);
    }
}
