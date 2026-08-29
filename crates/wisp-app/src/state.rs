use std::time::Instant;

use wisp_config::OverlayConfig;
use wisp_protocol::RenderModel;

use crate::layout::OverlayPlacement;

pub(crate) struct OverlayState {
    pub(crate) model: RenderModel,
    pub(crate) visible: bool,
    pub(crate) suppressed_until_new_request: Option<(String, u64)>,
    pub(crate) terminal_active: bool,
    pub(crate) description_scroll_started: Instant,
    pub(crate) scroll_accumulator: f32,
    pub(crate) placement: OverlayPlacement,
}

impl OverlayState {
    pub(crate) fn new(
        model: RenderModel,
        terminal_active: bool,
        placement: OverlayPlacement,
    ) -> Self {
        let visible = terminal_active && model_has_content(&model);
        Self {
            model,
            visible,
            suppressed_until_new_request: None,
            terminal_active,
            description_scroll_started: Instant::now(),
            scroll_accumulator: 0.0,
            placement,
        }
    }

    pub(crate) fn set_terminal_active(&mut self, active: bool) {
        self.terminal_active = active;
        if !active {
            self.suppressed_until_new_request =
                Some((self.model.session_id.clone(), self.model.request_id));
        }
        self.visible = active
            && model_has_content(&self.model)
            && !model_is_suppressed(&self.model, self.suppressed_until_new_request.as_ref());
    }

    pub(crate) fn render(&mut self, model: RenderModel, placement: OverlayPlacement) {
        if !self.terminal_active {
            self.suppressed_until_new_request = Some((model.session_id.clone(), model.request_id));
        }
        let suppressed = model_is_suppressed(&model, self.suppressed_until_new_request.as_ref());
        self.visible = self.terminal_active && model_has_content(&model) && !suppressed;
        if self.terminal_active && !suppressed {
            self.suppressed_until_new_request = None;
        }
        self.model = model;
        self.placement = placement;
        self.description_scroll_started = Instant::now();
    }

    pub(crate) fn hide(&mut self, session_id: &str) -> bool {
        if self.model.session_id != session_id {
            return false;
        }
        self.visible = false;
        true
    }

    pub(crate) fn scroll_candidates(
        &mut self,
        config: &OverlayConfig,
        delta_y: f32,
    ) -> Option<(String, usize)> {
        if !self.visible || self.model.candidates.len() < 2 || delta_y == 0.0 {
            return None;
        }
        if self.scroll_accumulator != 0.0 && self.scroll_accumulator.signum() != delta_y.signum() {
            self.scroll_accumulator = 0.0;
        }
        self.scroll_accumulator += delta_y;
        if self.scroll_accumulator.abs() < config.scroll_step_pixels {
            return None;
        }
        let steps = (self.scroll_accumulator.abs() / config.scroll_step_pixels).floor() as usize;
        self.scroll_accumulator %= config.scroll_step_pixels;
        let next = candidate_index_after_scroll(
            self.model.selected,
            self.model.candidates.len(),
            delta_y,
            steps,
        );
        if next == self.model.selected {
            return None;
        }
        self.model.selected = next;
        self.description_scroll_started = Instant::now();
        Some((self.model.session_id.clone(), next))
    }
}

pub(crate) fn model_has_content(model: &RenderModel) -> bool {
    !model.candidates.is_empty() || model.ghost_text.is_some()
}

fn model_is_suppressed(model: &RenderModel, suppression: Option<&(String, u64)>) -> bool {
    suppression.is_some_and(|(session_id, request_id)| {
        model.session_id == *session_id && model.request_id <= *request_id
    })
}

fn candidate_index_after_scroll(
    selected: usize,
    candidate_count: usize,
    delta_y: f32,
    steps: usize,
) -> usize {
    if candidate_count == 0 {
        return 0;
    }
    if delta_y < 0.0 {
        selected
            .saturating_add(steps)
            .min(candidate_count.saturating_sub(1))
    } else {
        selected.saturating_sub(steps)
    }
}

#[cfg(test)]
mod tests {
    use wisp_protocol::{Candidate, CandidateKind};

    use super::*;

    fn model(session_id: &str, request_id: u64) -> RenderModel {
        RenderModel {
            request_id,
            session_id: session_id.into(),
            anchor: None,
            candidates: Vec::new(),
            selected: 0,
            ghost_text: None,
        }
    }

    fn candidate() -> Candidate {
        Candidate {
            label: "candidate".into(),
            insert_text: "candidate".into(),
            description: None,
            kind: CandidateKind::Command,
            priority: 50.0,
            score: 1.0,
            replace_start: 0,
            replace_end: 0,
        }
    }

    #[test]
    fn empty_model_is_not_visible() {
        assert!(!model_has_content(&model("current", 1)));
    }

    #[test]
    fn hidden_message_only_applies_to_current_session() {
        let mut state = OverlayState::new(model("current", 1), true, OverlayPlacement::Below);
        assert!(!state.hide("stale"));
        assert!(state.hide("current"));
    }

    #[test]
    fn blurred_request_stays_suppressed_until_a_new_request() {
        let mut current = model("current", 7);
        current.candidates.push(candidate());
        let mut state = OverlayState::new(current, true, OverlayPlacement::Below);
        state.set_terminal_active(false);
        state.set_terminal_active(true);
        assert!(!state.visible);

        let mut old = model("current", 6);
        old.candidates.push(candidate());
        state.render(old, OverlayPlacement::Below);
        assert!(!state.visible);

        let mut fresh = model("current", 8);
        fresh.candidates.push(candidate());
        state.render(fresh, OverlayPlacement::Below);
        assert!(state.visible);
    }

    #[test]
    fn mouse_scroll_moves_selection_without_wrapping() {
        assert_eq!(candidate_index_after_scroll(0, 12, -1.0, 1), 1);
        assert_eq!(candidate_index_after_scroll(7, 12, -1.0, 3), 10);
        assert_eq!(candidate_index_after_scroll(11, 12, -1.0, 4), 11);
        assert_eq!(candidate_index_after_scroll(4, 12, 1.0, 2), 2);
        assert_eq!(candidate_index_after_scroll(0, 12, 1.0, 2), 0);
    }
}
