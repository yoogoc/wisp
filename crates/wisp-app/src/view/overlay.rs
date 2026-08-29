use std::sync::{Arc, mpsc};

use gpui::{
    Context, IntoElement, Render, ScrollWheelEvent, Window, WindowHandle, div, prelude::*, px,
    rgba, size,
};
use tracing::debug;
use wisp_config::OverlayConfig;

use super::CandidateDetailView;
use crate::{
    ipc::OverlayAction,
    layout::{OverlayPlacement, candidate_detail_height, overlay_height, visible_candidate_range},
    platform::{placement_height_limit, reposition_detail_window, set_detail_window_visible},
    presentation::{
        adaptive_label_width, candidate_kind_icon, description_scroll_offset,
        display_candidate_description, display_candidate_label,
    },
    state::OverlayState,
};

pub(crate) struct OverlayView {
    pub(crate) state: OverlayState,
    pub(crate) config: Arc<OverlayConfig>,
    pub(crate) detail_window: WindowHandle<CandidateDetailView>,
    pub(crate) interaction_sender: mpsc::Sender<OverlayAction>,
}

impl OverlayView {
    fn scroll_candidates(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(self.config.row_height));
        let Some((session_id, index)) = self
            .state
            .scroll_candidates(&self.config, f32::from(delta.y))
        else {
            return;
        };
        let _ = self
            .interaction_sender
            .send(OverlayAction::Select { session_id, index });
        cx.notify();
    }
}

impl Render for OverlayView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.state.model.selected;
        let list_height = overlay_height(&self.config, &self.state.model);
        let visible =
            visible_candidate_range(&self.config, self.state.model.candidates.len(), selected);
        let candidates = self
            .state
            .model
            .candidates
            .iter()
            .enumerate()
            .skip(visible.start)
            .take(visible.len())
            .map(|(index, candidate)| (index, candidate.clone()))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(index, candidate)| {
                let label = display_candidate_label(&self.config, &candidate.label, candidate.kind);
                let description = candidate
                    .description
                    .as_deref()
                    .map(display_candidate_description)
                    .filter(|description| !description.is_empty());
                let has_description = description.is_some();
                let label_width = adaptive_label_width(&self.config, &label);
                let description_width = self.config.row_content_width() - label_width;
                let description_offset = description.as_deref().map_or(0.0, |text| {
                    if index == selected {
                        description_scroll_offset(
                            &self.config,
                            text,
                            description_width,
                            self.state.description_scroll_started.elapsed(),
                        )
                    } else {
                        0.0
                    }
                });
                let detail_candidate = candidate.clone();
                div()
                    .id(("candidate", index))
                    .flex()
                    .items_center()
                    .h(px(self.config.row_height))
                    .px(px(self.config.row_horizontal_padding))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_size(px(13.0))
                    .text_color(rgba(0xe7e9efff))
                    .hover(|row| row.bg(rgba(0x293247ee)))
                    .when(index == selected, |row| row.bg(rgba(0x3d67b1cc)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(self.config.icon_width))
                            .mr(px(self.config.icon_gap))
                            .flex_shrink_0()
                            .text_size(px(15.0))
                            .text_color(rgba(0x8fa5cfff))
                            .child(candidate_kind_icon(candidate.kind)),
                    )
                    .child(
                        div()
                            .min_w(px(0.0))
                            .truncate()
                            .when(has_description, |text| {
                                text.w(px(label_width)).flex_shrink_0()
                            })
                            .when(!has_description, |text| text.flex_1())
                            .child(label),
                    )
                    .when_some(description, |row, description| {
                        row.child(
                            div()
                                .ml(px(self.config.description_gap))
                                .w(px(description_width))
                                .flex_shrink_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_color(rgba(0x8b91a1ff))
                                .when(description_offset > 0.0, |text| {
                                    text.child(
                                        div()
                                            .relative()
                                            .left(px(-description_offset))
                                            .whitespace_nowrap()
                                            .child(description.clone()),
                                    )
                                })
                                .when(description_offset == 0.0, |text| {
                                    text.truncate().child(description)
                                }),
                        )
                    })
                    .on_hover(cx.listener(move |view, hovered: &bool, _window, cx| {
                        if !*hovered {
                            return;
                        }
                        let detail_window = view.detail_window;
                        let model = view.state.model.clone();
                        let placement = view.state.placement;
                        let config = Arc::clone(&view.config);
                        let candidate = detail_candidate.clone();
                        if let Err(error) = detail_window.update(cx, |detail, window, cx| {
                            detail.candidate = candidate;
                            let height = candidate_detail_height(&config, &detail.candidate)
                                .min(placement_height_limit(&config, &model, placement));
                            window.resize(size(px(config.detail_width), px(height)));
                            if let Err(error) =
                                reposition_detail_window(window, &config, &model, height, placement)
                            {
                                debug!(%error, "could not position completion detail window");
                            }
                            if let Err(error) = set_detail_window_visible(window, true) {
                                debug!(%error, "could not show completion detail window");
                            }
                            cx.notify();
                        }) {
                            debug!(%error, "could not update completion detail window");
                        }
                    }))
            });

        let list = div()
            .flex()
            .flex_col()
            .w_full()
            .h(px(list_height))
            .overflow_hidden()
            .rounded(px(9.0))
            .border_1()
            .border_color(rgba(0x515768dd))
            .bg(rgba(0x17191fee))
            .shadow_lg()
            .when_some(self.state.model.ghost_text.clone(), |root, ghost| {
                root.child(
                    div()
                        .h(px(self.config.ghost_row_height))
                        .px(px(self.config.row_horizontal_padding))
                        .flex()
                        .items_center()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(rgba(0x9da4b5ff))
                        .child(format!("→ {ghost}")),
                )
            })
            .children(candidates);

        div()
            .id("overlay-root")
            .when(!self.state.visible, |root| root.opacity(0.0))
            .flex()
            .flex_col()
            .when(self.state.placement == OverlayPlacement::Above, |root| {
                root.justify_end()
            })
            .size_full()
            .on_scroll_wheel(cx.listener(|view, event, _, cx| {
                view.scroll_candidates(event, cx);
            }))
            .on_hover(cx.listener(|view, hovered: &bool, _window, cx| {
                if *hovered {
                    return;
                }
                if let Err(error) = view.detail_window.update(cx, |_, window, _| {
                    if let Err(error) = set_detail_window_visible(window, false) {
                        debug!(%error, "could not hide completion detail window");
                    }
                }) {
                    debug!(%error, "could not update completion detail visibility");
                }
            }))
            .child(list)
    }
}
