#[cfg(not(target_os = "macos"))]
use gpui::Window;
#[cfg(not(target_os = "macos"))]
use wisp_config::OverlayConfig;
#[cfg(not(target_os = "macos"))]
use wisp_protocol::RenderModel;

#[cfg(not(target_os = "macos"))]
use crate::layout::OverlayPlacement;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub(crate) use macos::{
    configure_application, install_status_item, placement_height_limit, preferred_model_placement,
    reposition_detail_window, reposition_overlay_window, set_detail_window_visible,
    set_overlay_window_visible, terminal_is_frontmost,
};

#[cfg(not(target_os = "macos"))]
pub(crate) fn configure_application() -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn install_status_item() -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn terminal_is_frontmost(_expected_application_id: Option<&str>) -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn preferred_model_placement(
    _config: &OverlayConfig,
    _model: &RenderModel,
) -> OverlayPlacement {
    OverlayPlacement::Below
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn placement_height_limit(
    _config: &OverlayConfig,
    _model: &RenderModel,
    _placement: OverlayPlacement,
) -> f32 {
    f32::MAX
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn reposition_overlay_window(
    _window: &Window,
    _config: &OverlayConfig,
    _model: &RenderModel,
    _height: f32,
    _placement: OverlayPlacement,
) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn reposition_detail_window(
    _window: &Window,
    _config: &OverlayConfig,
    _model: &RenderModel,
    _height: f32,
    _placement: OverlayPlacement,
) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn set_overlay_window_visible(_window: &Window, _visible: bool) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn set_detail_window_visible(_window: &Window, _visible: bool) -> anyhow::Result<()> {
    Ok(())
}
