use std::time::Duration;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use wisp_config::OverlayConfig;
use wisp_protocol::{CandidateKind, RenderModel};

pub(crate) fn display_candidate_label(
    config: &OverlayConfig,
    label: &str,
    kind: CandidateKind,
) -> String {
    if matches!(kind, CandidateKind::File | CandidateKind::Directory) {
        truncate_middle(label, config.max_path_label_width)
    } else {
        label.to_owned()
    }
}

pub(crate) fn candidate_kind_icon(kind: CandidateKind) -> &'static str {
    match kind {
        CandidateKind::Command => "⌘",
        CandidateKind::Subcommand => "›",
        CandidateKind::Option => "⚙",
        CandidateKind::File => "📄",
        CandidateKind::Directory => "📁",
        CandidateKind::Branch => "⑂",
        CandidateKind::History => "↺",
        CandidateKind::Ai => "✦",
    }
}

pub(crate) fn display_candidate_description(description: &str) -> String {
    description.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn adaptive_label_width(config: &OverlayConfig, label: &str) -> f32 {
    (approximate_text_width(config, label) + config.icon_gap)
        .clamp(config.min_label_width, config.max_label_width)
}

pub(crate) fn description_viewport_width(config: &OverlayConfig, label: &str) -> f32 {
    config.row_content_width() - adaptive_label_width(config, label)
}

pub(crate) fn description_scroll_offset(
    config: &OverlayConfig,
    description: &str,
    viewport_width: f32,
    elapsed: Duration,
) -> f32 {
    let distance = (approximate_text_width(config, description) - viewport_width).max(0.0);
    let start_pause = Duration::from_millis(config.description_scroll_pause_ms);
    let end_pause = Duration::from_millis(config.description_scroll_end_pause_ms);
    if distance == 0.0 || elapsed < start_pause {
        return 0.0;
    }
    let travel = Duration::from_secs_f32(distance / config.description_scroll_speed);
    let cycle = start_pause + travel + end_pause;
    let cycle_elapsed = elapsed.as_secs_f32() % cycle.as_secs_f32();
    let moving_at = cycle_elapsed - start_pause.as_secs_f32();
    if moving_at <= 0.0 {
        0.0
    } else {
        (moving_at * config.description_scroll_speed).min(distance)
    }
}

pub(crate) fn selected_description_overflows(config: &OverlayConfig, model: &RenderModel) -> bool {
    model
        .candidates
        .get(model.selected)
        .and_then(|candidate| {
            candidate.description.as_deref().map(|description| {
                let label = display_candidate_label(config, &candidate.label, candidate.kind);
                approximate_text_width(config, &display_candidate_description(description))
                    > description_viewport_width(config, &label)
            })
        })
        .unwrap_or(false)
}

fn approximate_text_width(config: &OverlayConfig, value: &str) -> f32 {
    UnicodeWidthStr::width(value) as f32 * config.text_column_width
}

fn truncate_middle(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_owned();
    }
    if max_width <= 1 {
        return "…".into();
    }

    let remaining = max_width - 1;
    let prefix_width = remaining / 3;
    let suffix_width = remaining - prefix_width;
    let prefix = take_prefix_columns(value, prefix_width);
    let suffix = take_suffix_columns(value, suffix_width);
    format!("{prefix}…{suffix}")
}

fn take_prefix_columns(value: &str, max_width: usize) -> String {
    let mut width = 0;
    value
        .chars()
        .take_while(|ch| {
            let next = width + ch.width().unwrap_or(0);
            let take = next <= max_width;
            if take {
                width = next;
            }
            take
        })
        .collect()
}

fn take_suffix_columns(value: &str, max_width: usize) -> String {
    let mut width = 0;
    let mut chars = value
        .chars()
        .rev()
        .take_while(|ch| {
            let next = width + ch.width().unwrap_or(0);
            let take = next <= max_width;
            if take {
                width = next;
            }
            take
        })
        .collect::<Vec<_>>();
    chars.reverse();
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OverlayConfig {
        OverlayConfig::default()
    }

    #[test]
    fn long_path_labels_are_shortened_without_changing_other_candidates() {
        let config = config();
        let path = "workspace/a-very-long-directory-name/another-directory/source-file.rs";
        let shortened = display_candidate_label(&config, path, CandidateKind::File);

        assert!(shortened.contains('…'));
        assert!(shortened.ends_with("source-file.rs"));
        assert!(UnicodeWidthStr::width(shortened.as_str()) <= config.max_path_label_width);
        assert_eq!(
            display_candidate_label(&config, path, CandidateKind::Command),
            path
        );
        assert_eq!(
            display_candidate_label(
                &config,
                "目录/非常非常非常长的文件名称.rs",
                CandidateKind::File
            ),
            "目录/非常非常非常长的文件名称.rs"
        );
    }

    #[test]
    fn candidate_types_use_compact_icons() {
        assert_eq!(candidate_kind_icon(CandidateKind::Command), "⌘");
        assert_eq!(candidate_kind_icon(CandidateKind::Subcommand), "›");
        assert_eq!(candidate_kind_icon(CandidateKind::Option), "⚙");
        assert_eq!(candidate_kind_icon(CandidateKind::File), "📄");
        assert_eq!(candidate_kind_icon(CandidateKind::Directory), "📁");
        assert_eq!(candidate_kind_icon(CandidateKind::Branch), "⑂");
        assert_eq!(candidate_kind_icon(CandidateKind::History), "↺");
        assert_eq!(candidate_kind_icon(CandidateKind::Ai), "✦");
    }

    #[test]
    fn short_commands_leave_more_room_for_descriptions() {
        let config = config();
        assert!(
            description_viewport_width(&config, "git")
                > description_viewport_width(
                    &config,
                    "an-extremely-long-command-name-that-needs-clipping"
                )
        );
        assert!(description_viewport_width(&config, "git") > 300.0);
    }

    #[test]
    fn selected_long_description_scrolls_after_a_pause() {
        let config = config();
        let description = "A very long completion description that cannot fit in its viewport";
        let width = 120.0;
        assert_eq!(
            description_scroll_offset(&config, description, width, Duration::ZERO),
            0.0
        );
        assert!(
            description_scroll_offset(&config, description, width, Duration::from_secs(2)) > 0.0
        );
    }

    #[test]
    fn description_control_whitespace_is_collapsed_to_one_line() {
        assert_eq!(
            display_candidate_description("first line\r\nsecond\tline   tail"),
            "first line second line tail"
        );
        assert_eq!(display_candidate_description("\n\r\t"), "");
    }
}
