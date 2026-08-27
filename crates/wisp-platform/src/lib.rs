use std::{
    path::PathBuf,
    process::Command,
    str::FromStr,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use wisp_core::display_cursor;
use wisp_protocol::{BufferSnapshot, CursorAnchor, ScreenPoint, TerminalKind, TerminalViewport};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowFrame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlacrittyInsets {
    pub titlebar: f32,
    pub padding_x: f32,
    pub padding_y: f32,
}

impl Default for AlacrittyInsets {
    fn default() -> Self {
        Self {
            titlebar: 28.0,
            padding_x: 0.0,
            padding_y: 0.0,
        }
    }
}

impl AlacrittyInsets {
    pub fn from_environment() -> Self {
        let defaults = Self::default();
        Self {
            titlebar: env_f32("WISP_ALACRITTY_TITLEBAR")
                .or_else(configured_alacritty_titlebar)
                .unwrap_or(defaults.titlebar),
            padding_x: env_f32("WISP_ALACRITTY_PADDING_X").unwrap_or(defaults.padding_x),
            padding_y: env_f32("WISP_ALACRITTY_PADDING_Y").unwrap_or(defaults.padding_y),
        }
    }
}

fn configured_alacritty_titlebar() -> Option<f32> {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("ALACRITTY_CONFIG_FILE") {
        paths.push(PathBuf::from(path));
    }
    if let Some(directory) = std::env::var_os("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(directory).join("alacritty/alacritty.toml"));
    }
    if let Some(directory) = std::env::var_os("HOME") {
        let home = PathBuf::from(directory);
        paths.push(home.join(".config/alacritty/alacritty.toml"));
        paths.push(home.join(".alacritty.toml"));
    }
    paths.into_iter().find_map(|path| {
        let source = std::fs::read_to_string(path).ok()?;
        titlebar_from_config(&source)
    })
}

fn titlebar_from_config(source: &str) -> Option<f32> {
    let config: toml::Value = toml::from_str(source).ok()?;
    let decorations = config
        .get("window")?
        .get("decorations")?
        .as_str()?
        .to_ascii_lowercase();
    Some(if decorations == "none" { 0.0 } else { 28.0 })
}

fn env_f32(name: &str) -> Option<f32> {
    std::env::var(name).ok()?.parse().ok()
}

#[derive(Debug, thiserror::Error)]
pub enum CursorLocationError {
    #[error("unsupported terminal")]
    UnsupportedTerminal,
    #[error("could not find the active Alacritty window: {0}")]
    WindowUnavailable(String),
    #[error("invalid terminal geometry")]
    InvalidGeometry,
}

pub trait WindowFrameProvider: Send + Sync {
    fn active_alacritty_window(&self) -> Result<WindowFrame, CursorLocationError>;
}

pub struct AlacrittyCursorLocator<P = SystemWindowFrameProvider> {
    frame_provider: P,
    insets: AlacrittyInsets,
}

impl Default for AlacrittyCursorLocator<SystemWindowFrameProvider> {
    fn default() -> Self {
        Self {
            frame_provider: SystemWindowFrameProvider,
            insets: AlacrittyInsets::from_environment(),
        }
    }
}

impl<P: WindowFrameProvider> AlacrittyCursorLocator<P> {
    pub fn new(frame_provider: P, insets: AlacrittyInsets) -> Self {
        Self {
            frame_provider,
            insets,
        }
    }

    pub fn locate(&self, snapshot: &BufferSnapshot) -> Result<CursorAnchor, CursorLocationError> {
        if snapshot.terminal.kind != TerminalKind::Alacritty {
            return Err(CursorLocationError::UnsupportedTerminal);
        }
        let columns = snapshot.terminal.columns;
        let rows = snapshot.terminal.rows;
        if columns == 0 || rows == 0 {
            return Err(CursorLocationError::InvalidGeometry);
        }
        let frame = self.frame_provider.active_alacritty_window()?;
        let viewport = snapshot.terminal.viewport.unwrap_or(TerminalViewport {
            x: 0,
            y: 0,
            grid_columns: columns,
            grid_rows: rows,
        });
        if viewport.grid_columns == 0 || viewport.grid_rows == 0 {
            return Err(CursorLocationError::InvalidGeometry);
        }
        let content_width = frame.width - self.insets.padding_x * 2.0;
        let content_height = frame.height - self.insets.titlebar - self.insets.padding_y * 2.0;
        if content_width <= 0.0 || content_height <= 0.0 {
            return Err(CursorLocationError::InvalidGeometry);
        }

        let (estimated_row, estimated_column) = estimated_grid_cursor(snapshot);
        let (cursor_row, cursor_column) = match (
            snapshot.terminal.cursor_row,
            snapshot.terminal.cursor_column,
        ) {
            (Some(row), Some(column)) => corrected_reported_cursor(snapshot, row, column),
            (Some(row), None) => (corrected_reported_row(snapshot, row), estimated_column),
            _ => (estimated_row, estimated_column),
        };
        let cursor_row = cursor_row.min(rows - 1);
        let cursor_column = cursor_column.min(columns - 1);

        let screen_column = viewport.x.saturating_add(cursor_column);
        let screen_row = viewport.y.saturating_add(cursor_row);
        let cell_width = content_width / f32::from(viewport.grid_columns);
        let line_height = content_height / f32::from(viewport.grid_rows);
        Ok(CursorAnchor {
            position: ScreenPoint {
                x: frame.x + self.insets.padding_x + f32::from(screen_column) * cell_width,
                y: frame.y
                    + self.insets.titlebar
                    + self.insets.padding_y
                    + f32::from(screen_row + 1) * line_height,
            },
            line_height,
            cell_width,
        })
    }
}

fn corrected_reported_row(snapshot: &BufferSnapshot, reported_row: u16) -> u16 {
    let Some(rendered) = snapshot.terminal.rendered.as_ref() else {
        return reported_row;
    };
    let current_before_cursor: String = snapshot.buffer.chars().take(snapshot.cursor).collect();
    let rendered_before_cursor: String = rendered.buffer.chars().take(rendered.cursor).collect();
    let (current_relative_row, _) = display_cursor(
        &snapshot.terminal.prompt,
        &current_before_cursor,
        snapshot.terminal.columns,
    );
    let (rendered_relative_row, _) =
        display_cursor(&rendered.prompt, &rendered_before_cursor, rendered.columns);
    let corrected = i32::from(reported_row) + i32::from(current_relative_row)
        - i32::from(rendered_relative_row);
    corrected.clamp(0, i32::from(snapshot.terminal.rows.saturating_sub(1))) as u16
}

fn corrected_reported_cursor(
    snapshot: &BufferSnapshot,
    reported_row: u16,
    reported_column: u16,
) -> (u16, u16) {
    let Some(rendered) = snapshot.terminal.rendered.as_ref() else {
        return (reported_row, reported_column);
    };
    if rendered.columns != snapshot.terminal.columns {
        return (
            corrected_reported_row(snapshot, reported_row),
            estimated_grid_cursor(snapshot).1,
        );
    }

    let columns = i64::from(snapshot.terminal.columns.max(1));
    let current_before_cursor: String = snapshot.buffer.chars().take(snapshot.cursor).collect();
    let rendered_before_cursor: String = rendered.buffer.chars().take(rendered.cursor).collect();
    let (current_row, current_column) = display_cursor(
        &snapshot.terminal.prompt,
        &current_before_cursor,
        snapshot.terminal.columns,
    );
    let (rendered_row, rendered_column) =
        display_cursor(&rendered.prompt, &rendered_before_cursor, rendered.columns);
    let current_linear = i64::from(current_row) * columns + i64::from(current_column);
    let rendered_linear = i64::from(rendered_row) * columns + i64::from(rendered_column);
    let reported_linear = i64::from(reported_row) * columns + i64::from(reported_column);
    let corrected_linear = (reported_linear + current_linear - rendered_linear).max(0);
    let row = (corrected_linear.div_euclid(columns))
        .min(i64::from(snapshot.terminal.rows.saturating_sub(1))) as u16;
    let column = corrected_linear.rem_euclid(columns) as u16;
    (row, column)
}

fn estimated_grid_cursor(snapshot: &BufferSnapshot) -> (u16, u16) {
    let before_cursor: String = snapshot.buffer.chars().take(snapshot.cursor).collect();
    let (cursor_relative_row, cursor_column) = display_cursor(
        &snapshot.terminal.prompt,
        &before_cursor,
        snapshot.terminal.columns,
    );
    let (final_relative_row, _) = display_cursor(
        &snapshot.terminal.prompt,
        &snapshot.buffer,
        snapshot.terminal.columns,
    );
    let lines_after_cursor = final_relative_row.saturating_sub(cursor_relative_row);
    let cursor_row = snapshot
        .terminal
        .rows
        .saturating_sub(1)
        .saturating_sub(lines_after_cursor)
        .min(snapshot.terminal.rows - 1);
    (cursor_row, cursor_column)
}

pub struct SystemWindowFrameProvider;

impl WindowFrameProvider for SystemWindowFrameProvider {
    fn active_alacritty_window(&self) -> Result<WindowFrame, CursorLocationError> {
        if let Ok(value) = std::env::var("WISP_ALACRITTY_BOUNDS") {
            return parse_frame(&value).ok_or(CursorLocationError::InvalidGeometry);
        }
        static CACHE: OnceLock<Mutex<Option<(Instant, WindowFrame)>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(None));
        if let Some((created_at, frame)) = *cache.lock().expect("window frame cache mutex poisoned")
            && created_at.elapsed() < Duration::from_millis(250)
        {
            return Ok(frame);
        }
        let frame = active_alacritty_window()?;
        *cache.lock().expect("window frame cache mutex poisoned") = Some((Instant::now(), frame));
        Ok(frame)
    }
}

#[cfg(target_os = "macos")]
fn active_alacritty_window() -> Result<WindowFrame, CursorLocationError> {
    let script = r#"tell application "System Events" to tell process "Alacritty" to get {position, size} of front window"#;
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .output()
        .map_err(|error| CursorLocationError::WindowUnavailable(error.to_string()))?;
    if !output.status.success() {
        return Err(CursorLocationError::WindowUnavailable(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    parse_frame(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        CursorLocationError::WindowUnavailable("unexpected System Events response".into())
    })
}

#[cfg(not(target_os = "macos"))]
fn active_alacritty_window() -> Result<WindowFrame, CursorLocationError> {
    Err(CursorLocationError::WindowUnavailable(
        "automatic Alacritty window discovery currently requires macOS".into(),
    ))
}

fn parse_frame(value: &str) -> Option<WindowFrame> {
    let values: Vec<f32> = value
        .split([',', ' ', '\n'])
        .filter(|part| !part.trim().is_empty())
        .filter_map(|part| f32::from_str(part.trim()).ok())
        .collect();
    if values.len() != 4 {
        return None;
    }
    Some(WindowFrame {
        x: values[0],
        y: values[1],
        width: values[2],
        height: values[3],
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use wisp_protocol::{ShellKind, TerminalSnapshot};

    use super::*;

    struct FixedFrame;

    impl WindowFrameProvider for FixedFrame {
        fn active_alacritty_window(&self) -> Result<WindowFrame, CursorLocationError> {
            Ok(WindowFrame {
                x: 100.0,
                y: 200.0,
                width: 800.0,
                height: 508.0,
            })
        }
    }

    #[test]
    fn maps_alacritty_grid_to_screen_coordinates() {
        let locator = AlacrittyCursorLocator::new(
            FixedFrame,
            AlacrittyInsets {
                titlebar: 28.0,
                padding_x: 0.0,
                padding_y: 0.0,
            },
        );
        let snapshot = BufferSnapshot {
            request_id: 1,
            session_id: "test".into(),
            buffer: "git".into(),
            cursor: 3,
            cwd: PathBuf::from("/tmp"),
            shell: ShellKind::Zsh,
            terminal: TerminalSnapshot {
                kind: TerminalKind::Alacritty,
                window_id: None,
                columns: 80,
                rows: 24,
                prompt: "$ ".into(),
                cursor_row: None,
                cursor_column: None,
                rendered: None,
                viewport: None,
            },
        };
        let anchor = locator.locate(&snapshot).unwrap();
        assert_eq!(anchor.cell_width, 10.0);
        assert_eq!(anchor.line_height, 20.0);
        assert_eq!(anchor.position, ScreenPoint { x: 150.0, y: 708.0 });
    }

    #[test]
    fn reported_grid_cursor_overrides_bottom_row_estimate() {
        let locator = AlacrittyCursorLocator::new(
            FixedFrame,
            AlacrittyInsets {
                titlebar: 28.0,
                padding_x: 0.0,
                padding_y: 0.0,
            },
        );
        let snapshot = BufferSnapshot {
            request_id: 1,
            session_id: "test".into(),
            buffer: "git".into(),
            cursor: 3,
            cwd: PathBuf::from("/tmp"),
            shell: ShellKind::Zsh,
            terminal: TerminalSnapshot {
                kind: TerminalKind::Alacritty,
                window_id: None,
                columns: 80,
                rows: 24,
                prompt: "$ ".into(),
                cursor_row: Some(4),
                cursor_column: Some(9),
                rendered: None,
                viewport: None,
            },
        };

        let anchor = locator.locate(&snapshot).unwrap();

        assert_eq!(anchor.position, ScreenPoint { x: 190.0, y: 328.0 });
    }

    #[test]
    fn maps_zellij_pane_cursor_through_full_terminal_grid() {
        let locator = AlacrittyCursorLocator::new(
            FixedFrame,
            AlacrittyInsets {
                titlebar: 28.0,
                padding_x: 0.0,
                padding_y: 0.0,
            },
        );
        let snapshot = BufferSnapshot {
            request_id: 1,
            session_id: "test".into(),
            buffer: "git".into(),
            cursor: 3,
            cwd: PathBuf::from("/tmp"),
            shell: ShellKind::Zsh,
            terminal: TerminalSnapshot {
                kind: TerminalKind::Alacritty,
                window_id: None,
                columns: 38,
                rows: 20,
                prompt: "$ ".into(),
                cursor_row: Some(4),
                cursor_column: Some(10),
                rendered: None,
                viewport: Some(TerminalViewport {
                    x: 40,
                    y: 2,
                    grid_columns: 80,
                    grid_rows: 24,
                }),
            },
        };

        let anchor = locator.locate(&snapshot).unwrap();

        assert_eq!(anchor.cell_width, 10.0);
        assert_eq!(anchor.line_height, 20.0);
        assert_eq!(anchor.position, ScreenPoint { x: 600.0, y: 368.0 });
    }

    #[test]
    fn reported_row_is_advanced_when_new_buffer_wraps() {
        let locator = AlacrittyCursorLocator::new(
            FixedFrame,
            AlacrittyInsets {
                titlebar: 28.0,
                padding_x: 0.0,
                padding_y: 0.0,
            },
        );
        let snapshot = BufferSnapshot {
            request_id: 1,
            session_id: "test".into(),
            buffer: "12345678".into(),
            cursor: 8,
            cwd: PathBuf::from("/tmp"),
            shell: ShellKind::Zsh,
            terminal: TerminalSnapshot {
                kind: TerminalKind::Alacritty,
                window_id: None,
                columns: 10,
                rows: 24,
                prompt: "$ ".into(),
                cursor_row: Some(4),
                cursor_column: Some(9),
                rendered: Some(wisp_protocol::RenderedCursorSnapshot {
                    buffer: "1234567".into(),
                    cursor: 7,
                    prompt: "$ ".into(),
                    columns: 10,
                }),
                viewport: None,
            },
        };

        let anchor = locator.locate(&snapshot).unwrap();

        assert_eq!(anchor.position, ScreenPoint { x: 100.0, y: 348.0 });
    }

    #[test]
    fn reported_column_prevents_prompt_error_from_accumulating_on_long_lines() {
        let mut snapshot = BufferSnapshot {
            request_id: 1,
            session_id: "test".into(),
            buffer: "12345678".into(),
            cursor: 8,
            cwd: PathBuf::from("/tmp"),
            shell: ShellKind::Zsh,
            terminal: TerminalSnapshot {
                kind: TerminalKind::Alacritty,
                window_id: None,
                columns: 10,
                rows: 24,
                prompt: ">>> ".into(),
                cursor_row: Some(4),
                cursor_column: Some(9),
                rendered: Some(wisp_protocol::RenderedCursorSnapshot {
                    buffer: "1234567".into(),
                    cursor: 7,
                    prompt: ">>> ".into(),
                    columns: 10,
                }),
                viewport: None,
            },
        };

        assert_eq!(corrected_reported_cursor(&snapshot, 4, 9), (5, 0));
        snapshot.buffer.push('9');
        snapshot.cursor += 1;
        assert_eq!(corrected_reported_cursor(&snapshot, 4, 9), (5, 1));
    }

    #[test]
    fn reported_row_is_advanced_for_explicit_newline() {
        let mut snapshot = BufferSnapshot {
            request_id: 1,
            session_id: "test".into(),
            buffer: "echo one\necho two".into(),
            cursor: 17,
            cwd: PathBuf::from("/tmp"),
            shell: ShellKind::Zsh,
            terminal: TerminalSnapshot {
                kind: TerminalKind::Alacritty,
                window_id: None,
                columns: 80,
                rows: 24,
                prompt: "$ ".into(),
                cursor_row: Some(8),
                cursor_column: None,
                rendered: Some(wisp_protocol::RenderedCursorSnapshot {
                    buffer: "echo one".into(),
                    cursor: 8,
                    prompt: "$ ".into(),
                    columns: 80,
                }),
                viewport: None,
            },
        };

        assert_eq!(corrected_reported_row(&snapshot, 8), 9);
        snapshot.terminal.cursor_row = Some(23);
        assert_eq!(corrected_reported_row(&snapshot, 23), 23);
    }

    #[test]
    fn parses_system_events_frame() {
        assert_eq!(
            parse_frame("100, 200, 800, 600\n"),
            Some(WindowFrame {
                x: 100.0,
                y: 200.0,
                width: 800.0,
                height: 600.0,
            })
        );
    }

    #[test]
    fn detects_decorationless_alacritty_config() {
        assert_eq!(
            titlebar_from_config("[window]\ndecorations = \"none\"\n"),
            Some(0.0)
        );
        assert_eq!(
            titlebar_from_config("[window]\ndecorations = \"Full\"\n"),
            Some(28.0)
        );
    }
}
