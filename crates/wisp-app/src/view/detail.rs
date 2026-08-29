use gpui::{Context, IntoElement, Render, Window, div, prelude::*, px, rgba};
use wisp_protocol::{Candidate, CandidateKind};

use crate::presentation::{candidate_kind_icon, display_candidate_description};

#[derive(Clone)]
pub(crate) struct CandidateDetailView {
    pub(crate) candidate: Candidate,
}

impl CandidateDetailView {
    pub(crate) fn empty() -> Self {
        Self {
            candidate: Candidate {
                label: String::new(),
                insert_text: String::new(),
                description: None,
                kind: CandidateKind::Command,
                priority: 50.0,
                score: 0.0,
                replace_start: 0,
                replace_end: 0,
            },
        }
    }
}

impl Render for CandidateDetailView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let description = self
            .candidate
            .description
            .as_deref()
            .map(display_candidate_description)
            .filter(|description| !description.is_empty());
        let insert_text = (self.candidate.insert_text != self.candidate.label)
            .then(|| self.candidate.insert_text.clone());

        div()
            .id("candidate-detail")
            .flex()
            .flex_col()
            .gap(px(6.0))
            .size_full()
            .overflow_y_scroll()
            .p(px(10.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgba(0x515768ff))
            .bg(rgba(0x17191fff))
            .shadow_lg()
            .text_size(px(12.0))
            .text_color(rgba(0xe7e9efff))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(candidate_kind_icon(self.candidate.kind))
                    .child(
                        div()
                            .min_w(px(0.0))
                            .whitespace_normal()
                            .child(self.candidate.label.clone()),
                    ),
            )
            .when_some(description, |detail, description| {
                detail.child(
                    div()
                        .whitespace_normal()
                        .text_color(rgba(0xaeb4c2ff))
                        .child(description),
                )
            })
            .when_some(insert_text, |detail, insert_text| {
                detail.child(
                    div()
                        .whitespace_normal()
                        .text_color(rgba(0x7f8799ff))
                        .child(format!("插入：{insert_text}")),
                )
            })
    }
}
