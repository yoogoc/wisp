mod detail;
mod overlay;

use gpui::{App, WindowHandle};
use tracing::debug;

pub(crate) use detail::CandidateDetailView;
pub(crate) use overlay::OverlayView;

use crate::platform::set_detail_window_visible;

pub(crate) fn hide_detail_window(detail_window: WindowHandle<CandidateDetailView>, cx: &mut App) {
    if let Err(error) = detail_window.update(cx, |_, window, _| {
        if let Err(error) = set_detail_window_visible(window, false) {
            debug!(%error, "could not hide completion detail window");
        }
    }) {
        debug!(%error, "could not update completion detail visibility");
    }
}
