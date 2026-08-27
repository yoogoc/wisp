use std::{collections::HashMap, path::Path};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WispConfig {
    #[serde(default)]
    pub completion: CompletionConfig,
    #[serde(default)]
    pub generator: GeneratorConfig,
    #[serde(default)]
    pub ai: AiRuntimeConfig,
    #[serde(default)]
    pub overlay: OverlayConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub terminal: TerminalConfig,
    #[serde(default)]
    pub startup: StartupConfig,
    pub default_provider: Option<String>,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

impl WispConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("read Wisp config {}", path.display()))?;
        let config: Self = toml::from_str(&source)
            .with_context(|| format!("parse Wisp config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.generator.timeout_ms == 0 || self.generator.max_output_bytes == 0 {
            bail!("generator timeout_ms and max_output_bytes must be greater than zero");
        }
        if self.overlay.max_visible_candidates == 0
            || self.overlay.max_path_label_width == 0
            || self.overlay.width <= 0.0
            || self.overlay.row_height <= 0.0
            || self.overlay.text_column_width <= 0.0
            || self.overlay.description_scroll_speed <= 0.0
            || self.overlay.scroll_step_pixels <= 0.0
            || self.overlay.detail_width <= 0.0
            || self.overlay.detail_columns == 0
            || self.overlay.frame_interval_ms == 0
            || self.overlay.activity_check_ms == 0
            || self.overlay.reconnect_ms == 0
        {
            bail!("overlay sizes, speeds, and max_visible_candidates must be greater than zero");
        }
        if self.overlay.min_label_width > self.overlay.max_label_width {
            bail!("overlay min_label_width cannot exceed max_label_width");
        }
        if self.overlay.row_content_width() <= self.overlay.min_label_width
            || self.overlay.detail_min_height > self.overlay.detail_max_height
        {
            bail!("overlay content width and detail height range are invalid");
        }
        if self.ai.max_output_chars == 0
            || self.ai.max_tokens == 0
            || self.ai.provider_error_chars == 0
            || !(0.0..=2.0).contains(&self.ai.temperature)
        {
            bail!("AI output limits must be positive and temperature must be between 0 and 2");
        }
        if self.daemon.overlay_channel_capacity == 0
            || self.startup.attempts == 0
            || self.startup.retry_ms == 0
        {
            bail!("daemon channel capacity and startup retry values must be greater than zero");
        }
        for (id, provider) in &self.providers {
            let timeout_ms = match provider {
                ProviderConfig::OpenaiCompatible { timeout_ms, .. }
                | ProviderConfig::Process { timeout_ms, .. } => *timeout_ms,
            };
            if timeout_ms == 0 {
                bail!("provider `{id}` timeout_ms must be greater than zero");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CompletionConfig {
    /// Zero keeps every matching candidate.
    #[serde(default)]
    pub max_candidates: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneratorConfig {
    #[serde(default = "default_generator_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_generator_max_output_bytes")]
    pub max_output_bytes: usize,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            timeout_ms: default_generator_timeout_ms(),
            max_output_bytes: default_generator_max_output_bytes(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AiRuntimeConfig {
    #[serde(default = "default_ai_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_ai_min_command_chars")]
    pub min_command_chars: usize,
    #[serde(default = "default_ai_max_output_chars")]
    pub max_output_chars: usize,
    #[serde(default = "default_ai_temperature")]
    pub temperature: f32,
    #[serde(default = "default_ai_max_tokens")]
    pub max_tokens: u16,
    #[serde(default = "default_provider_error_chars")]
    pub provider_error_chars: usize,
}

impl Default for AiRuntimeConfig {
    fn default() -> Self {
        Self {
            debounce_ms: default_ai_debounce_ms(),
            min_command_chars: default_ai_min_command_chars(),
            max_output_chars: default_ai_max_output_chars(),
            temperature: default_ai_temperature(),
            max_tokens: default_ai_max_tokens(),
            provider_error_chars: default_provider_error_chars(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OverlayConfig {
    #[serde(default = "default_max_visible_candidates")]
    pub max_visible_candidates: usize,
    #[serde(default = "default_max_path_label_width")]
    pub max_path_label_width: usize,
    #[serde(default = "default_overlay_width")]
    pub width: f32,
    #[serde(default = "default_row_horizontal_padding")]
    pub row_horizontal_padding: f32,
    #[serde(default = "default_icon_width")]
    pub icon_width: f32,
    #[serde(default = "default_icon_gap")]
    pub icon_gap: f32,
    #[serde(default = "default_description_gap")]
    pub description_gap: f32,
    #[serde(default = "default_min_label_width")]
    pub min_label_width: f32,
    #[serde(default = "default_max_label_width")]
    pub max_label_width: f32,
    #[serde(default = "default_text_column_width")]
    pub text_column_width: f32,
    #[serde(default = "default_description_scroll_pause_ms")]
    pub description_scroll_pause_ms: u64,
    #[serde(default = "default_description_scroll_end_pause_ms")]
    pub description_scroll_end_pause_ms: u64,
    #[serde(default = "default_description_scroll_speed")]
    pub description_scroll_speed: f32,
    #[serde(default = "default_cursor_gap")]
    pub cursor_gap: f32,
    #[serde(default = "default_scroll_step_pixels")]
    pub scroll_step_pixels: f32,
    #[serde(default = "default_detail_width")]
    pub detail_width: f32,
    #[serde(default = "default_detail_window_gap")]
    pub detail_window_gap: f32,
    #[serde(default = "default_row_height")]
    pub row_height: f32,
    #[serde(default = "default_ghost_row_height")]
    pub ghost_row_height: f32,
    #[serde(default = "default_detail_min_height")]
    pub detail_min_height: f32,
    #[serde(default = "default_detail_max_height")]
    pub detail_max_height: f32,
    #[serde(default = "default_detail_line_height")]
    pub detail_line_height: f32,
    #[serde(default = "default_detail_columns")]
    pub detail_columns: usize,
    #[serde(default = "default_frame_interval_ms")]
    pub frame_interval_ms: u64,
    #[serde(default = "default_activity_check_ms")]
    pub activity_check_ms: u64,
    #[serde(default = "default_reconnect_ms")]
    pub reconnect_ms: u64,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            max_visible_candidates: default_max_visible_candidates(),
            max_path_label_width: default_max_path_label_width(),
            width: default_overlay_width(),
            row_horizontal_padding: default_row_horizontal_padding(),
            icon_width: default_icon_width(),
            icon_gap: default_icon_gap(),
            description_gap: default_description_gap(),
            min_label_width: default_min_label_width(),
            max_label_width: default_max_label_width(),
            text_column_width: default_text_column_width(),
            description_scroll_pause_ms: default_description_scroll_pause_ms(),
            description_scroll_end_pause_ms: default_description_scroll_end_pause_ms(),
            description_scroll_speed: default_description_scroll_speed(),
            cursor_gap: default_cursor_gap(),
            scroll_step_pixels: default_scroll_step_pixels(),
            detail_width: default_detail_width(),
            detail_window_gap: default_detail_window_gap(),
            row_height: default_row_height(),
            ghost_row_height: default_ghost_row_height(),
            detail_min_height: default_detail_min_height(),
            detail_max_height: default_detail_max_height(),
            detail_line_height: default_detail_line_height(),
            detail_columns: default_detail_columns(),
            frame_interval_ms: default_frame_interval_ms(),
            activity_check_ms: default_activity_check_ms(),
            reconnect_ms: default_reconnect_ms(),
        }
    }
}

impl OverlayConfig {
    pub fn row_content_width(&self) -> f32 {
        self.width
            - self.row_horizontal_padding * 2.0
            - self.icon_width
            - self.icon_gap
            - self.description_gap
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_overlay_channel_capacity")]
    pub overlay_channel_capacity: usize,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            overlay_channel_capacity: default_overlay_channel_capacity(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TerminalConfig {
    #[serde(default = "default_titlebar")]
    pub alacritty_titlebar: f32,
    #[serde(default)]
    pub alacritty_padding_x: f32,
    #[serde(default)]
    pub alacritty_padding_y: f32,
    #[serde(default = "default_window_frame_cache_ms")]
    pub window_frame_cache_ms: u64,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            alacritty_titlebar: default_titlebar(),
            alacritty_padding_x: 0.0,
            alacritty_padding_y: 0.0,
            window_frame_cache_ms: default_window_frame_cache_ms(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartupConfig {
    #[serde(default = "default_startup_attempts")]
    pub attempts: usize,
    #[serde(default = "default_startup_retry_ms")]
    pub retry_ms: u64,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            attempts: default_startup_attempts(),
            retry_ms: default_startup_retry_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ProviderConfig {
    OpenaiCompatible {
        base_url: String,
        model: String,
        api_key_env: Option<String>,
        #[serde(default = "default_provider_timeout_ms")]
        timeout_ms: u64,
    },
    Process {
        command: Vec<String>,
        #[serde(default = "default_provider_timeout_ms")]
        timeout_ms: u64,
    },
}

const fn default_generator_timeout_ms() -> u64 {
    150
}
const fn default_generator_max_output_bytes() -> usize {
    256 * 1024
}
const fn default_ai_debounce_ms() -> u64 {
    120
}
const fn default_ai_min_command_chars() -> usize {
    3
}
const fn default_ai_max_output_chars() -> usize {
    256
}
fn default_ai_temperature() -> f32 {
    0.1
}
const fn default_ai_max_tokens() -> u16 {
    64
}
const fn default_provider_error_chars() -> usize {
    512
}
const fn default_max_visible_candidates() -> usize {
    8
}
const fn default_max_path_label_width() -> usize {
    40
}
fn default_overlay_width() -> f32 {
    440.0
}
fn default_row_horizontal_padding() -> f32 {
    10.0
}
fn default_icon_width() -> f32 {
    24.0
}
fn default_icon_gap() -> f32 {
    8.0
}
fn default_description_gap() -> f32 {
    12.0
}
fn default_min_label_width() -> f32 {
    48.0
}
fn default_max_label_width() -> f32 {
    220.0
}
fn default_text_column_width() -> f32 {
    7.0
}
const fn default_description_scroll_pause_ms() -> u64 {
    700
}
const fn default_description_scroll_end_pause_ms() -> u64 {
    500
}
fn default_description_scroll_speed() -> f32 {
    34.0
}
fn default_cursor_gap() -> f32 {
    8.0
}
fn default_scroll_step_pixels() -> f32 {
    18.0
}
fn default_detail_width() -> f32 {
    400.0
}
fn default_detail_window_gap() -> f32 {
    8.0
}
fn default_row_height() -> f32 {
    32.0
}
fn default_ghost_row_height() -> f32 {
    30.0
}
fn default_detail_min_height() -> f32 {
    96.0
}
fn default_detail_max_height() -> f32 {
    560.0
}
fn default_detail_line_height() -> f32 {
    18.0
}
const fn default_detail_columns() -> usize {
    54
}
const fn default_frame_interval_ms() -> u64 {
    16
}
const fn default_activity_check_ms() -> u64 {
    100
}
const fn default_reconnect_ms() -> u64 {
    250
}
const fn default_overlay_channel_capacity() -> usize {
    64
}
fn default_titlebar() -> f32 {
    28.0
}
const fn default_window_frame_cache_ms() -> u64 {
    250
}
const fn default_startup_attempts() -> usize {
    20
}
const fn default_startup_retry_ms() -> u64 {
    50
}
const fn default_provider_timeout_ms() -> u64 {
    800
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_current_runtime_defaults() {
        let config: WispConfig = toml::from_str("").unwrap();
        assert_eq!(config.completion.max_candidates, 0);
        assert_eq!(config.generator.timeout_ms, 150);
        assert_eq!(config.overlay.max_visible_candidates, 8);
        assert_eq!(config.overlay.width, 440.0);
        assert_eq!(config.terminal.window_frame_cache_ms, 250);
    }

    #[test]
    fn invalid_overlay_values_are_rejected() {
        let mut config = WispConfig::default();
        config.overlay.max_visible_candidates = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn documented_example_is_complete_and_valid() {
        let config: WispConfig =
            toml::from_str(include_str!("../../../config.example.toml")).unwrap();
        config.validate().unwrap();
        assert_eq!(config.generator.max_output_bytes, 262_144);
        assert_eq!(config.overlay.detail_columns, 54);
        assert_eq!(config.startup.attempts, 20);
    }
}
