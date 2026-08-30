use std::ops::Range;

use gpui::{Pixels, Point, point, px};
use unicode_width::UnicodeWidthStr;
use wisp_config::OverlayConfig;
use wisp_protocol::{Candidate, CursorAnchor, RenderModel};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlayPlacement {
    Above,
    Below,
}

pub(crate) fn candidate_detail_height(config: &OverlayConfig, candidate: &Candidate) -> f32 {
    let lines = [
        candidate.label.as_str(),
        candidate.description.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .map(|text| {
        UnicodeWidthStr::width(text)
            .div_ceil(config.detail_columns)
            .max(1)
    })
    .sum::<usize>()
        + usize::from(candidate.insert_text != candidate.label);
    (config.row_height + lines as f32 * config.detail_line_height)
        .clamp(config.detail_min_height, config.detail_max_height)
}

pub(crate) fn overlay_height(config: &OverlayConfig, model: &RenderModel) -> f32 {
    let rows = model
        .candidates
        .len()
        .clamp(1, config.max_visible_candidates) as f32;
    rows * config.row_height
        + if model.ghost_text.is_some() {
            config.ghost_row_height
        } else {
            0.0
        }
}

pub(crate) fn maximum_overlay_height(config: &OverlayConfig, model: &RenderModel) -> f32 {
    model
        .candidates
        .iter()
        .map(|candidate| candidate_detail_height(config, candidate))
        .fold(overlay_height(config, model), f32::max)
}

pub(crate) fn visible_candidate_range(
    config: &OverlayConfig,
    candidate_count: usize,
    selected: usize,
) -> Range<usize> {
    let visible_count = candidate_count.min(config.max_visible_candidates);
    let selected = selected.min(candidate_count.saturating_sub(1));
    let start = selected
        .saturating_add(1)
        .saturating_sub(visible_count)
        .min(candidate_count.saturating_sub(visible_count));
    start..start + visible_count
}

pub(crate) fn overlay_origin(config: &OverlayConfig, model: &RenderModel) -> Point<Pixels> {
    model.anchor.map_or(point(px(120.0), px(120.0)), |anchor| {
        point(
            px(anchor.position.x),
            px(anchor.position.y + config.cursor_gap),
        )
    })
}

pub(crate) fn preferred_overlay_placement(
    config: &OverlayConfig,
    anchor: CursorAnchor,
    required_height: f32,
    screen_height: f32,
) -> OverlayPlacement {
    let below = anchor.position.y + config.cursor_gap;
    let below_available = (screen_height - below).max(0.0);
    let above_available = (anchor.position.y - anchor.line_height - config.cursor_gap).max(0.0);
    if required_height <= below_available {
        OverlayPlacement::Below
    } else if required_height <= above_available {
        OverlayPlacement::Above
    } else if below_available >= above_available {
        OverlayPlacement::Below
    } else {
        OverlayPlacement::Above
    }
}

pub(crate) fn overlay_top_for_placement(
    config: &OverlayConfig,
    anchor: CursorAnchor,
    height: f32,
    placement: OverlayPlacement,
) -> f32 {
    match placement {
        OverlayPlacement::Below => anchor.position.y + config.cursor_gap,
        OverlayPlacement::Above => {
            (anchor.position.y - anchor.line_height - config.cursor_gap - height).max(0.0)
        }
    }
}

#[cfg(test)]
fn preferred_overlay_top(
    config: &OverlayConfig,
    anchor: CursorAnchor,
    height: f32,
    screen_height: f32,
) -> f32 {
    match preferred_overlay_placement(config, anchor, height, screen_height) {
        OverlayPlacement::Below => anchor.position.y + config.cursor_gap,
        OverlayPlacement::Above => {
            (anchor.position.y - anchor.line_height - config.cursor_gap - height).max(0.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use wisp_protocol::{CandidateKind, ScreenPoint};

    use super::*;

    fn anchor() -> CursorAnchor {
        CursorAnchor {
            position: ScreenPoint { x: 320.0, y: 480.0 },
            line_height: 20.0,
            cell_width: 10.0,
        }
    }

    fn empty_model() -> RenderModel {
        RenderModel {
            request_id: 1,
            session_id: "current".into(),
            terminal_application_id: None,
            anchor: None,
            candidates: Vec::new(),
            selected: 0,
            ghost_text: None,
        }
    }

    #[test]
    fn overlay_has_padding_below_cursor() {
        let config = OverlayConfig::default();
        let mut model = empty_model();
        model.anchor = Some(anchor());
        assert_eq!(overlay_origin(&config, &model), point(px(320.0), px(488.0)));
    }

    #[test]
    fn candidate_viewport_follows_selection() {
        let config = OverlayConfig::default();
        assert_eq!(visible_candidate_range(&config, 0, 0), 0..0);
        assert_eq!(visible_candidate_range(&config, 3, 2), 0..3);
        assert_eq!(visible_candidate_range(&config, 12, 0), 0..8);
        assert_eq!(visible_candidate_range(&config, 12, 7), 0..8);
        assert_eq!(visible_candidate_range(&config, 12, 8), 1..9);
        assert_eq!(visible_candidate_range(&config, 12, 11), 4..12);
    }

    #[test]
    fn long_candidate_details_expand_the_native_window() {
        let config = OverlayConfig::default();
        let short = Candidate {
            label: "git".into(),
            insert_text: "git".into(),
            description: Some("version control".into()),
            kind: CandidateKind::Command,
            priority: 50.0,
            score: 1.0,
            replace_start: 0,
            replace_end: 3,
        };
        let mut long = short.clone();
        long.description = Some("a detailed explanation ".repeat(100));
        assert!(candidate_detail_height(&config, &long) > candidate_detail_height(&config, &short));
        assert_eq!(candidate_detail_height(&config, &long), 560.0);
    }

    #[test]
    fn overlay_flips_above_cursor_when_below_does_not_fit() {
        let config = OverlayConfig::default();
        assert_eq!(
            preferred_overlay_top(&config, anchor(), 160.0, 1000.0),
            488.0
        );
        assert_eq!(
            preferred_overlay_top(&config, anchor(), 160.0, 600.0),
            292.0
        );
    }

    #[test]
    fn locked_detail_placement_never_crosses_the_cursor_line() {
        let config = OverlayConfig::default();
        let below_top =
            overlay_top_for_placement(&config, anchor(), 320.0, OverlayPlacement::Below);
        assert!(below_top >= anchor().position.y + config.cursor_gap);

        let above_top =
            overlay_top_for_placement(&config, anchor(), 320.0, OverlayPlacement::Above);
        assert!(
            above_top + 320.0 <= anchor().position.y - anchor().line_height - config.cursor_gap
        );
    }

    #[test]
    fn detail_height_is_used_before_choosing_a_side() {
        let config = OverlayConfig::default();
        let mut anchor = anchor();
        anchor.position.y = 500.0;
        assert_eq!(
            preferred_overlay_placement(&config, anchor, 420.0, 700.0),
            OverlayPlacement::Above
        );
    }
}
