use std::{
    path::PathBuf,
    process::Command,
    str::FromStr,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use wisp_config::TerminalConfig;
use wisp_core::display_cursor;
use wisp_protocol::{BufferSnapshot, CursorAnchor, ScreenPoint, TerminalKind, TerminalViewport};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowFrame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveWindow {
    pub application_id: String,
    pub frame: WindowFrame,
    /// Exact focused terminal text area reported by macOS Accessibility.
    pub content_frame: Option<WindowFrame>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocatedCursor {
    pub anchor: CursorAnchor,
    pub application_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalInsets {
    pub titlebar: f32,
    pub padding_x: f32,
    pub padding_y: f32,
}

impl Default for TerminalInsets {
    fn default() -> Self {
        let config = TerminalConfig::default();
        Self {
            titlebar: config.alacritty_titlebar,
            padding_x: config.alacritty_padding_x,
            padding_y: config.alacritty_padding_y,
        }
    }
}

impl TerminalInsets {
    pub fn from_environment() -> Self {
        Self::from_config(&TerminalConfig::default())
    }

    pub fn from_config(config: &TerminalConfig) -> Self {
        Self {
            titlebar: env_f32("WISP_ALACRITTY_TITLEBAR")
                .or_else(|| configured_alacritty_titlebar(config.alacritty_titlebar))
                .unwrap_or(config.alacritty_titlebar),
            padding_x: env_f32("WISP_ALACRITTY_PADDING_X").unwrap_or(config.alacritty_padding_x),
            padding_y: env_f32("WISP_ALACRITTY_PADDING_Y").unwrap_or(config.alacritty_padding_y),
        }
    }

    pub fn for_terminal(kind: TerminalKind, config: &TerminalConfig) -> Self {
        if kind == TerminalKind::Alacritty {
            return Self::from_config(config);
        }
        let prefix = match kind {
            TerminalKind::AppleTerminal => "APPLE_TERMINAL",
            TerminalKind::Iterm2 => "ITERM2",
            TerminalKind::Ghostty => "GHOSTTY",
            TerminalKind::Wezterm => "WEZTERM",
            TerminalKind::Kitty => "KITTY",
            TerminalKind::Warp => "WARP",
            TerminalKind::Vscode => "VSCODE",
            TerminalKind::Unknown => "FALLBACK",
            TerminalKind::Alacritty => unreachable!(),
        };
        let default_titlebar = match kind {
            // Warp and VS Code reserve additional chrome above their terminal grid.
            TerminalKind::Warp => 52.0,
            TerminalKind::Vscode => 86.0,
            _ => config.fallback_titlebar,
        };
        Self {
            titlebar: env_f32(&format!("WISP_{prefix}_TITLEBAR")).unwrap_or(default_titlebar),
            padding_x: env_f32(&format!("WISP_{prefix}_PADDING_X"))
                .unwrap_or(config.fallback_padding_x),
            padding_y: env_f32(&format!("WISP_{prefix}_PADDING_Y"))
                .unwrap_or(config.fallback_padding_y),
        }
    }
}

fn configured_alacritty_titlebar(default_titlebar: f32) -> Option<f32> {
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
        titlebar_from_config(&source, default_titlebar)
    })
}

fn titlebar_from_config(source: &str, default_titlebar: f32) -> Option<f32> {
    let config: toml::Value = toml::from_str(source).ok()?;
    let decorations = config
        .get("window")?
        .get("decorations")?
        .as_str()?
        .to_ascii_lowercase();
    Some(if decorations == "none" {
        0.0
    } else {
        default_titlebar
    })
}

fn env_f32(name: &str) -> Option<f32> {
    std::env::var(name).ok()?.parse().ok()
}

#[derive(Debug, thiserror::Error)]
pub enum CursorLocationError {
    #[error("unsupported terminal")]
    UnsupportedTerminal,
    #[error("could not find the active terminal window: {0}")]
    WindowUnavailable(String),
    #[error("invalid terminal geometry")]
    InvalidGeometry,
}

pub trait WindowFrameProvider: Send + Sync {
    fn active_window(&self, terminal: TerminalKind) -> Result<ActiveWindow, CursorLocationError>;

    fn active_window_for_grid(
        &self,
        terminal: TerminalKind,
        _grid_columns: u16,
        _grid_rows: u16,
    ) -> Result<ActiveWindow, CursorLocationError> {
        self.active_window(terminal)
    }
}

pub struct TerminalCursorLocator<P = SystemWindowFrameProvider> {
    frame_provider: P,
    config: TerminalConfig,
    fixed_insets: Option<TerminalInsets>,
}

impl Default for TerminalCursorLocator<SystemWindowFrameProvider> {
    fn default() -> Self {
        Self {
            frame_provider: SystemWindowFrameProvider::default(),
            config: TerminalConfig::default(),
            fixed_insets: None,
        }
    }
}

impl<P: WindowFrameProvider> TerminalCursorLocator<P> {
    pub fn new(frame_provider: P, insets: TerminalInsets) -> Self {
        Self {
            frame_provider,
            config: TerminalConfig::default(),
            fixed_insets: Some(insets),
        }
    }

    pub fn locate(&self, snapshot: &BufferSnapshot) -> Result<CursorAnchor, CursorLocationError> {
        self.locate_with_context(snapshot)
            .map(|located| located.anchor)
    }

    pub fn locate_with_context(
        &self,
        snapshot: &BufferSnapshot,
    ) -> Result<LocatedCursor, CursorLocationError> {
        let columns = snapshot.terminal.columns;
        let rows = snapshot.terminal.rows;
        if columns == 0 || rows == 0 {
            return Err(CursorLocationError::InvalidGeometry);
        }
        let viewport = snapshot.terminal.viewport.unwrap_or(TerminalViewport {
            x: 0,
            y: 0,
            grid_columns: columns,
            grid_rows: rows,
        });
        if viewport.grid_columns == 0 || viewport.grid_rows == 0 {
            return Err(CursorLocationError::InvalidGeometry);
        }
        let active_window = self.frame_provider.active_window_for_grid(
            snapshot.terminal.kind,
            viewport.grid_columns,
            viewport.grid_rows,
        )?;
        let effective_terminal = if snapshot.terminal.kind == TerminalKind::Unknown {
            terminal_kind_for_application(&active_window.application_id)
                .unwrap_or(TerminalKind::Unknown)
        } else {
            snapshot.terminal.kind
        };
        let content_frame = active_window.content_frame.filter(|content| {
            content_frame_is_plausible(active_window.frame, *content, columns, rows)
        });
        let has_accessible_content_frame = content_frame.is_some();
        let frame = content_frame.unwrap_or(active_window.frame);
        let insets = if has_accessible_content_frame {
            TerminalInsets {
                titlebar: 0.0,
                padding_x: 0.0,
                padding_y: 0.0,
            }
        } else {
            self.fixed_insets
                .unwrap_or_else(|| TerminalInsets::for_terminal(effective_terminal, &self.config))
        };
        let content_width = frame.width - insets.padding_x * 2.0;
        let content_height = frame.height - insets.titlebar - insets.padding_y * 2.0;
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
        Ok(LocatedCursor {
            anchor: CursorAnchor {
                position: ScreenPoint {
                    x: frame.x + insets.padding_x + f32::from(screen_column) * cell_width,
                    y: frame.y
                        + insets.titlebar
                        + insets.padding_y
                        + f32::from(screen_row + 1) * line_height,
                },
                line_height,
                cell_width,
            },
            application_id: active_window.application_id,
        })
    }
}

fn content_frame_is_plausible(
    window: WindowFrame,
    content: WindowFrame,
    columns: u16,
    rows: u16,
) -> bool {
    const TOLERANCE: f32 = 2.0;
    content.width >= f32::from(columns) * 2.0
        && content.height >= f32::from(rows) * 2.0
        && content.x >= window.x - TOLERANCE
        && content.y >= window.y - TOLERANCE
        && content.x + content.width <= window.x + window.width + TOLERANCE
        && content.y + content.height <= window.y + window.height + TOLERANCE
}

impl TerminalCursorLocator<SystemWindowFrameProvider> {
    pub fn from_config(config: &TerminalConfig) -> Self {
        Self {
            frame_provider: SystemWindowFrameProvider {
                cache_ttl: Duration::from_millis(config.window_frame_cache_ms),
            },
            config: config.clone(),
            fixed_insets: None,
        }
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

pub struct SystemWindowFrameProvider {
    cache_ttl: Duration,
}

impl Default for SystemWindowFrameProvider {
    fn default() -> Self {
        Self {
            cache_ttl: Duration::from_millis(TerminalConfig::default().window_frame_cache_ms),
        }
    }
}

impl WindowFrameProvider for SystemWindowFrameProvider {
    fn active_window(&self, terminal: TerminalKind) -> Result<ActiveWindow, CursorLocationError> {
        self.active_window_cached(terminal, None)
    }

    fn active_window_for_grid(
        &self,
        terminal: TerminalKind,
        grid_columns: u16,
        grid_rows: u16,
    ) -> Result<ActiveWindow, CursorLocationError> {
        self.active_window_cached(terminal, Some((grid_columns, grid_rows)))
    }
}

impl SystemWindowFrameProvider {
    fn active_window_cached(
        &self,
        terminal: TerminalKind,
        grid: Option<(u16, u16)>,
    ) -> Result<ActiveWindow, CursorLocationError> {
        let bounds_variable = format!("WISP_{}_BOUNDS", terminal_env_prefix(terminal));
        if let Ok(value) = std::env::var(&bounds_variable) {
            let frame = parse_frame(&value).ok_or(CursorLocationError::InvalidGeometry)?;
            return Ok(ActiveWindow {
                application_id: primary_bundle_id(terminal)
                    .unwrap_or("wisp.fallback")
                    .into(),
                frame,
                content_frame: None,
            });
        }
        type CachedWindow = (Instant, Option<(u16, u16)>, ActiveWindow);
        static CACHE: OnceLock<Mutex<Option<CachedWindow>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(None));
        if let Some((created_at, cached_grid, active_window)) = cache
            .lock()
            .expect("window frame cache mutex poisoned")
            .as_ref()
            && created_at.elapsed() < self.cache_ttl
            && *cached_grid == grid
            && terminal_matches_application(terminal, &active_window.application_id)
        {
            return Ok(active_window.clone());
        }
        let active_window = active_terminal_window()?;
        if !terminal_matches_application(terminal, &active_window.application_id) {
            return Err(CursorLocationError::WindowUnavailable(format!(
                "frontmost application {} does not match {terminal:?}",
                active_window.application_id
            )));
        }
        *cache.lock().expect("window frame cache mutex poisoned") =
            Some((Instant::now(), grid, active_window.clone()));
        Ok(active_window)
    }
}

#[cfg(target_os = "macos")]
fn active_terminal_window() -> Result<ActiveWindow, CursorLocationError> {
    let script = r#"
tell application "System Events"
    set terminalProcess to first application process whose frontmost is true
    set applicationId to bundle identifier of terminalProcess
    set {windowX, windowY} to position of front window of terminalProcess
    set {windowWidth, windowHeight} to size of front window of terminalProcess
    set contentFrame to ""
    try
        set focusedElement to value of attribute "AXFocusedUIElement" of terminalProcess
        set focusedRole to value of attribute "AXRole" of focusedElement
        if focusedRole is "AXTextArea" then
            set {contentX, contentY} to position of focusedElement
            set {contentWidth, contentHeight} to size of focusedElement
            set contentFrame to contentX & "," & contentY & "," & contentWidth & "," & contentHeight
        end if
    end try
    return applicationId & linefeed & windowX & "," & windowY & "," & windowWidth & "," & windowHeight & linefeed & contentFrame
end tell
"#;
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .output()
        .map_err(|error| CursorLocationError::WindowUnavailable(error.to_string()))?;
    if !output.status.success() {
        return Err(CursorLocationError::WindowUnavailable(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    parse_active_window(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        CursorLocationError::WindowUnavailable("unexpected System Events response".into())
    })
}

#[cfg(not(target_os = "macos"))]
fn active_terminal_window() -> Result<ActiveWindow, CursorLocationError> {
    Err(CursorLocationError::WindowUnavailable(
        "automatic terminal window discovery currently requires macOS".into(),
    ))
}

fn parse_active_window(value: &str) -> Option<ActiveWindow> {
    let mut lines = value.lines();
    let application_id = lines.next()?.trim();
    let frame = parse_frame(lines.next()?)?;
    let content_frame = lines.next().and_then(parse_frame);
    Some(ActiveWindow {
        application_id: application_id.to_owned(),
        frame,
        content_frame,
    })
}

pub fn primary_bundle_id(terminal: TerminalKind) -> Option<&'static str> {
    match terminal {
        TerminalKind::Alacritty => Some("org.alacritty"),
        TerminalKind::AppleTerminal => Some("com.apple.Terminal"),
        TerminalKind::Iterm2 => Some("com.googlecode.iterm2"),
        TerminalKind::Ghostty => Some("com.mitchellh.ghostty"),
        TerminalKind::Wezterm => Some("com.github.wez.wezterm"),
        TerminalKind::Kitty => Some("net.kovidgoyal.kitty"),
        TerminalKind::Warp => Some("dev.warp.Warp-Stable"),
        TerminalKind::Vscode => Some("com.microsoft.VSCode"),
        TerminalKind::Unknown => None,
    }
}

fn terminal_matches_application(terminal: TerminalKind, application_id: &str) -> bool {
    match terminal {
        TerminalKind::Warp => matches!(
            application_id,
            "dev.warp.Warp-Stable" | "dev.warp.Warp-Beta" | "dev.warp.Warp-Nightly"
        ),
        TerminalKind::Vscode => matches!(
            application_id,
            "com.microsoft.VSCode" | "com.microsoft.VSCodeInsiders"
        ),
        TerminalKind::Unknown => terminal_kind_for_application(application_id).is_some(),
        _ => primary_bundle_id(terminal).is_some_and(|expected| expected == application_id),
    }
}

pub fn terminal_kind_for_application(application_id: &str) -> Option<TerminalKind> {
    [
        TerminalKind::Alacritty,
        TerminalKind::AppleTerminal,
        TerminalKind::Iterm2,
        TerminalKind::Ghostty,
        TerminalKind::Wezterm,
        TerminalKind::Kitty,
        TerminalKind::Warp,
        TerminalKind::Vscode,
    ]
    .into_iter()
    .find(|terminal| terminal_matches_application(*terminal, application_id))
}

fn terminal_env_prefix(terminal: TerminalKind) -> &'static str {
    match terminal {
        TerminalKind::Alacritty => "ALACRITTY",
        TerminalKind::AppleTerminal => "APPLE_TERMINAL",
        TerminalKind::Iterm2 => "ITERM2",
        TerminalKind::Ghostty => "GHOSTTY",
        TerminalKind::Wezterm => "WEZTERM",
        TerminalKind::Kitty => "KITTY",
        TerminalKind::Warp => "WARP",
        TerminalKind::Vscode => "VSCODE",
        TerminalKind::Unknown => "FALLBACK",
    }
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

    fn fixed_active_window(terminal: TerminalKind) -> ActiveWindow {
        ActiveWindow {
            application_id: primary_bundle_id(terminal)
                .unwrap_or("test.terminal")
                .into(),
            frame: WindowFrame {
                x: 100.0,
                y: 200.0,
                width: 800.0,
                height: 508.0,
            },
            content_frame: None,
        }
    }

    impl WindowFrameProvider for FixedFrame {
        fn active_window(
            &self,
            terminal: TerminalKind,
        ) -> Result<ActiveWindow, CursorLocationError> {
            Ok(fixed_active_window(terminal))
        }
    }

    struct GridRecordingFrame(Mutex<Option<(u16, u16)>>);

    impl WindowFrameProvider for GridRecordingFrame {
        fn active_window(
            &self,
            _terminal: TerminalKind,
        ) -> Result<ActiveWindow, CursorLocationError> {
            panic!("the locator should include terminal grid dimensions")
        }

        fn active_window_for_grid(
            &self,
            terminal: TerminalKind,
            grid_columns: u16,
            grid_rows: u16,
        ) -> Result<ActiveWindow, CursorLocationError> {
            *self.0.lock().unwrap() = Some((grid_columns, grid_rows));
            Ok(fixed_active_window(terminal))
        }
    }

    #[test]
    fn maps_alacritty_grid_to_screen_coordinates() {
        let locator = TerminalCursorLocator::new(
            FixedFrame,
            TerminalInsets {
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
    fn forwards_outer_grid_size_to_the_window_frame_cache() {
        let locator = TerminalCursorLocator::new(
            GridRecordingFrame(Mutex::new(None)),
            TerminalInsets {
                titlebar: 28.0,
                padding_x: 0.0,
                padding_y: 0.0,
            },
        );
        let snapshot = BufferSnapshot {
            request_id: 1,
            session_id: "resized".into(),
            buffer: "git".into(),
            cursor: 3,
            cwd: PathBuf::from("/tmp"),
            shell: ShellKind::Fish,
            terminal: TerminalSnapshot {
                kind: TerminalKind::Alacritty,
                window_id: None,
                columns: 72,
                rows: 20,
                prompt: String::new(),
                cursor_row: None,
                cursor_column: None,
                rendered: None,
                viewport: Some(TerminalViewport {
                    x: 0,
                    y: 0,
                    grid_columns: 144,
                    grid_rows: 40,
                }),
            },
        };

        locator.locate(&snapshot).unwrap();

        assert_eq!(*locator.frame_provider.0.lock().unwrap(), Some((144, 40)));
    }

    #[test]
    fn maps_every_supported_terminal_grid() {
        let locator = TerminalCursorLocator::new(
            FixedFrame,
            TerminalInsets {
                titlebar: 28.0,
                padding_x: 0.0,
                padding_y: 0.0,
            },
        );
        let mut snapshot = BufferSnapshot {
            request_id: 1,
            session_id: "test".into(),
            buffer: "git".into(),
            cursor: 3,
            cwd: PathBuf::from("/tmp"),
            shell: ShellKind::Zsh,
            terminal: TerminalSnapshot {
                kind: TerminalKind::Unknown,
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
        for terminal in [
            TerminalKind::Alacritty,
            TerminalKind::AppleTerminal,
            TerminalKind::Iterm2,
            TerminalKind::Ghostty,
            TerminalKind::Wezterm,
            TerminalKind::Kitty,
            TerminalKind::Warp,
            TerminalKind::Vscode,
        ] {
            snapshot.terminal.kind = terminal;
            assert!(locator.locate(&snapshot).is_ok(), "{terminal:?}");
        }
    }

    #[test]
    fn reported_grid_cursor_overrides_bottom_row_estimate() {
        let locator = TerminalCursorLocator::new(
            FixedFrame,
            TerminalInsets {
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
        let locator = TerminalCursorLocator::new(
            FixedFrame,
            TerminalInsets {
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
        let locator = TerminalCursorLocator::new(
            FixedFrame,
            TerminalInsets {
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
    fn parses_accessibility_content_frame() {
        assert_eq!(
            parse_active_window("com.apple.Terminal\n100,200,800,600\n110,240,780,550\n"),
            Some(ActiveWindow {
                application_id: "com.apple.Terminal".into(),
                frame: WindowFrame {
                    x: 100.0,
                    y: 200.0,
                    width: 800.0,
                    height: 600.0,
                },
                content_frame: Some(WindowFrame {
                    x: 110.0,
                    y: 240.0,
                    width: 780.0,
                    height: 550.0,
                }),
            })
        );
    }

    #[test]
    fn every_supported_terminal_has_a_bundle_identity() {
        for terminal in [
            TerminalKind::Alacritty,
            TerminalKind::AppleTerminal,
            TerminalKind::Iterm2,
            TerminalKind::Ghostty,
            TerminalKind::Wezterm,
            TerminalKind::Kitty,
            TerminalKind::Warp,
            TerminalKind::Vscode,
        ] {
            let application_id = primary_bundle_id(terminal).unwrap();
            assert!(terminal_matches_application(terminal, application_id));
            assert_eq!(
                terminal_kind_for_application(application_id),
                Some(terminal)
            );
            assert!(terminal_matches_application(
                TerminalKind::Unknown,
                application_id
            ));
        }
        assert!(!terminal_matches_application(
            TerminalKind::Unknown,
            "com.apple.Safari"
        ));
    }

    #[test]
    fn detects_decorationless_alacritty_config() {
        assert_eq!(
            titlebar_from_config("[window]\ndecorations = \"none\"\n", 28.0),
            Some(0.0)
        );
        assert_eq!(
            titlebar_from_config("[window]\ndecorations = \"Full\"\n", 28.0),
            Some(28.0)
        );
    }
}
