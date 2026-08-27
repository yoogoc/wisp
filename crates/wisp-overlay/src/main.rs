use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::Context;
use clap::Parser;
use gpui::{
    App, Application, Bounds, Context as GpuiContext, Entity, Render, ScrollWheelEvent, Timer,
    Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, div,
    point, prelude::*, px, rgba, size,
};
use tokio::net::UnixStream;
use tracing::debug;
use tracing_subscriber::EnvFilter;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use wisp_protocol::{
    Candidate, CandidateKind, ClientMessage, CursorAnchor, RenderModel, ServerMessage, framed,
    receive_message, send_message,
};

const MAX_VISIBLE_CANDIDATES: usize = 8;
const MAX_PATH_LABEL_WIDTH: usize = 40;
const OVERLAY_WIDTH: f32 = 440.0;
const ROW_CONTENT_WIDTH: f32 = OVERLAY_WIDTH - 20.0 - 24.0 - 8.0 - 12.0;
const MIN_LABEL_WIDTH: f32 = 48.0;
const MAX_LABEL_WIDTH: f32 = 220.0;
const APPROX_TEXT_COLUMN_WIDTH: f32 = 7.0;
const DESCRIPTION_SCROLL_PAUSE: Duration = Duration::from_millis(700);
const DESCRIPTION_SCROLL_END_PAUSE: Duration = Duration::from_millis(500);
const DESCRIPTION_SCROLL_SPEED: f32 = 34.0;
const CURSOR_GAP: f32 = 8.0;
const SCROLL_STEP_PIXELS: f32 = 18.0;
const DETAIL_WIDTH: f32 = 400.0;
const DETAIL_WINDOW_GAP: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayPlacement {
    Above,
    Below,
}

#[derive(Debug)]
enum OverlayAction {
    Select { session_id: String, index: usize },
}

#[derive(Debug, Parser)]
#[command(version, about = "Wisp GPUI autocomplete overlay")]
struct Args {
    #[arg(long, env = "WISP_SOCKET")]
    socket: Option<PathBuf>,
}

struct OverlayView {
    model: RenderModel,
    visible: bool,
    suppressed_until_new_request: Option<(String, u64)>,
    description_scroll_started: Instant,
    scroll_accumulator: f32,
    placement: OverlayPlacement,
    detail_window: WindowHandle<CandidateDetailView>,
    interaction_sender: mpsc::Sender<OverlayAction>,
}

#[derive(Clone)]
struct CandidateDetailView {
    candidate: Candidate,
}

impl Render for CandidateDetailView {
    fn render(&mut self, _window: &mut Window, _cx: &mut GpuiContext<Self>) -> impl IntoElement {
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

impl OverlayView {
    fn scroll_candidates(&mut self, event: &ScrollWheelEvent, cx: &mut GpuiContext<Self>) {
        if !self.visible || self.model.candidates.len() < 2 {
            return;
        }
        let delta = event.delta.pixel_delta(px(32.0));
        let y = f32::from(delta.y);
        if y == 0.0 {
            return;
        }
        if self.scroll_accumulator != 0.0 && self.scroll_accumulator.signum() != y.signum() {
            self.scroll_accumulator = 0.0;
        }
        self.scroll_accumulator += y;
        if self.scroll_accumulator.abs() < SCROLL_STEP_PIXELS {
            return;
        }
        let steps = (self.scroll_accumulator.abs() / SCROLL_STEP_PIXELS).floor() as usize;
        self.scroll_accumulator %= SCROLL_STEP_PIXELS;
        let next = candidate_index_after_scroll(
            self.model.selected,
            self.model.candidates.len(),
            y,
            steps,
        );
        if next == self.model.selected {
            return;
        }
        self.model.selected = next;
        self.description_scroll_started = Instant::now();
        let _ = self.interaction_sender.send(OverlayAction::Select {
            session_id: self.model.session_id.clone(),
            index: next,
        });
        cx.notify();
    }
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

fn hide_detail_window(detail_window: WindowHandle<CandidateDetailView>, cx: &mut App) {
    if let Err(error) = detail_window.update(cx, |_, window, _| {
        if let Err(error) = set_detail_window_visible(window, false) {
            debug!(%error, "could not hide completion detail window");
        }
    }) {
        debug!(%error, "could not update completion detail visibility");
    }
}

impl Render for OverlayView {
    fn render(&mut self, _window: &mut Window, cx: &mut GpuiContext<Self>) -> impl IntoElement {
        let selected = self.model.selected;
        let list_height = overlay_height(&self.model);
        let visible = visible_candidate_range(self.model.candidates.len(), selected);
        let candidates = self
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
                let label = display_candidate_label(&candidate.label, candidate.kind);
                let description = candidate
                    .description
                    .as_deref()
                    .map(display_candidate_description)
                    .filter(|description| !description.is_empty());
                let has_description = description.is_some();
                let label_width = adaptive_label_width(&label);
                let description_width = ROW_CONTENT_WIDTH - label_width;
                let description_offset = description.as_deref().map_or(0.0, |text| {
                    if index == selected {
                        description_scroll_offset(
                            text,
                            description_width,
                            self.description_scroll_started.elapsed(),
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
                    .h(px(32.0))
                    .px(px(10.0))
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
                            .w(px(24.0))
                            .mr(px(8.0))
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
                                .ml(px(12.0))
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
                        let model = view.model.clone();
                        let placement = view.placement;
                        let candidate = detail_candidate.clone();
                        if let Err(error) = detail_window.update(cx, |detail, window, cx| {
                            detail.candidate = candidate;
                            let height = candidate_detail_height(&detail.candidate)
                                .min(placement_height_limit(&model, placement));
                            window.resize(size(px(DETAIL_WIDTH), px(height)));
                            if let Err(error) =
                                reposition_detail_window(window, &model, height, placement)
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
            .when_some(self.model.ghost_text.clone(), |root, ghost| {
                root.child(
                    div()
                        .h(px(30.0))
                        .px(px(10.0))
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
            .when(!self.visible, |root| root.opacity(0.0))
            .flex()
            .flex_col()
            .when(self.placement == OverlayPlacement::Above, |root| {
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

fn candidate_detail_height(candidate: &Candidate) -> f32 {
    const DETAIL_COLUMNS: usize = 54;
    const DETAIL_LINE_HEIGHT: f32 = 18.0;
    let lines = [
        candidate.label.as_str(),
        candidate.description.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .map(|text| UnicodeWidthStr::width(text).div_ceil(DETAIL_COLUMNS).max(1))
    .sum::<usize>()
        + usize::from(candidate.insert_text != candidate.label);
    (32.0 + lines as f32 * DETAIL_LINE_HEIGHT).clamp(96.0, 560.0)
}

fn empty_detail_candidate() -> Candidate {
    Candidate {
        label: String::new(),
        insert_text: String::new(),
        description: None,
        kind: CandidateKind::Command,
        score: 0.0,
        replace_start: 0,
        replace_end: 0,
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "wisp=info".into()))
        .init();
    let args = Args::parse();
    let socket = args.socket.unwrap_or_else(default_socket_path);
    let (sender, receiver) = mpsc::channel();
    let (interaction_sender, interaction_receiver) = mpsc::channel();
    spawn_subscription(socket.clone(), sender);
    spawn_interaction_worker(socket, interaction_receiver);

    let first = loop {
        match receiver.recv().context("overlay subscription stopped")? {
            ServerMessage::Render { model } if model_has_content(&model) => break model,
            _ => continue,
        }
    };
    let first_placement = preferred_model_placement(&first);
    let receiver = Arc::new(Mutex::new(receiver));

    Application::new().run(move |cx: &mut App| {
        if let Err(error) = configure_overlay_application() {
            debug!(%error, "could not configure overlay application policy");
        }
        let terminal_active = alacritty_is_frontmost();
        let height = overlay_height(&first);
        let anchor = overlay_origin(&first, height);
        let bounds = Bounds {
            origin: anchor,
            size: size(px(OVERLAY_WIDTH), px(height)),
        };
        let detail_bounds = Bounds {
            origin: anchor,
            size: size(px(DETAIL_WIDTH), px(96.0)),
        };
        let detail_window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(detail_bounds)),
                    titlebar: None,
                    focus: false,
                    show: false,
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    is_resizable: false,
                    is_minimizable: false,
                    window_background: WindowBackgroundAppearance::Transparent,
                    window_decorations: None,
                    ..Default::default()
                },
                |window, cx| {
                    if let Err(error) = set_detail_window_visible(window, false) {
                        debug!(%error, "could not configure completion detail window");
                    }
                    cx.new(|_| CandidateDetailView {
                        candidate: empty_detail_candidate(),
                    })
                },
            )
            .expect("open Wisp completion detail window");
        let receiver = Arc::clone(&receiver);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: false,
                // GPUI otherwise orders the initial NSPanel before we have applied
                // the non-activating application/window configuration.
                show: false,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: None,
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|_| OverlayView {
                    model: first,
                    visible: terminal_active,
                    suppressed_until_new_request: None,
                    description_scroll_started: Instant::now(),
                    scroll_accumulator: 0.0,
                    placement: first_placement,
                    detail_window,
                    interaction_sender,
                });
                let initial_model = view.read(cx).model.clone();
                if let Err(error) = reposition_window(
                    window,
                    &initial_model,
                    overlay_height(&initial_model),
                    first_placement,
                ) {
                    debug!(%error, "could not position initial overlay window");
                }
                if let Err(error) = set_overlay_window_visible(window, terminal_active) {
                    debug!(%error, "could not configure overlay window");
                }
                poll_messages(window, cx, view.clone(), receiver, terminal_active);
                view
            },
        )
        .expect("open Wisp overlay window");
    });
    Ok(())
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn configure_overlay_application() -> anyhow::Result<()> {
    use cocoa::{
        appkit::{NSApp, NSApplication, NSApplicationActivationPolicyAccessory},
        base::{YES, nil},
    };
    use objc::{msg_send, sel, sel_impl};

    let application = unsafe { NSApp() };
    if application == nil {
        return Err(anyhow::anyhow!("AppKit application is unavailable"));
    }
    let changed =
        unsafe { application.setActivationPolicy_(NSApplicationActivationPolicyAccessory) };
    if changed != YES {
        return Err(anyhow::anyhow!(
            "AppKit rejected accessory activation policy"
        ));
    }
    let active: objc::runtime::BOOL = unsafe { msg_send![application, isActive] };
    if active == YES {
        unsafe {
            let _: () = msg_send![application, deactivate];
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn configure_overlay_application() -> anyhow::Result<()> {
    Ok(())
}

fn poll_messages(
    window: &mut Window,
    cx: &mut App,
    view: Entity<OverlayView>,
    receiver: Arc<Mutex<mpsc::Receiver<ServerMessage>>>,
    initial_terminal_active: bool,
) {
    window
        .spawn(cx, async move |cx| {
            let mut terminal_active = initial_terminal_active;
            let mut last_activity_check = Instant::now();
            let mut animate_description = initial_terminal_active
                && cx
                    .update(|_, app| selected_description_overflows(&view.read(app).model))
                    .unwrap_or(false);
            loop {
                Timer::after(Duration::from_millis(16)).await;
                let activity_changed =
                    if last_activity_check.elapsed() >= Duration::from_millis(100) {
                        last_activity_check = Instant::now();
                        let active = alacritty_is_frontmost();
                        let changed = active != terminal_active;
                        terminal_active = active;
                        changed
                    } else {
                        false
                    };
                let messages = {
                    let receiver = receiver.lock().expect("overlay receiver mutex poisoned");
                    receiver.try_iter().collect::<Vec<_>>()
                };
                if messages.is_empty() && !activity_changed && !animate_description {
                    continue;
                }
                if cx
                    .update(|window, app| {
                        if activity_changed {
                            hide_detail_window(view.read(app).detail_window, app);
                            let current = view.read(app);
                            let suppression = if terminal_active {
                                current.suppressed_until_new_request.clone()
                            } else {
                                Some((current.model.session_id.clone(), current.model.request_id))
                            };
                            let visible = terminal_active
                                && model_has_content(&current.model)
                                && !model_is_suppressed(&current.model, suppression.as_ref());
                            animate_description =
                                visible && selected_description_overflows(&current.model);
                            if let Err(error) = set_overlay_window_visible(window, visible) {
                                debug!(%error, "could not follow Alacritty activation");
                            }
                            view.update(app, |view, cx| {
                                view.visible = visible;
                                view.suppressed_until_new_request = suppression;
                                cx.notify();
                            });
                        }
                        for message in messages {
                            match message {
                                ServerMessage::Render { model } => {
                                    let mut suppression =
                                        view.read(app).suppressed_until_new_request.clone();
                                    if !terminal_active {
                                        suppression =
                                            Some((model.session_id.clone(), model.request_id));
                                    }
                                    let suppressed =
                                        model_is_suppressed(&model, suppression.as_ref());
                                    let visible =
                                        terminal_active && model_has_content(&model) && !suppressed;
                                    animate_description =
                                        visible && selected_description_overflows(&model);
                                    if terminal_active && !suppressed {
                                        suppression = None;
                                    }
                                    hide_detail_window(view.read(app).detail_window, app);
                                    let placement = preferred_model_placement(&model);
                                    let height = overlay_height(&model);
                                    window.resize(size(px(OVERLAY_WIDTH), px(height)));
                                    if let Err(error) =
                                        reposition_window(window, &model, height, placement)
                                    {
                                        debug!(%error, "could not reposition overlay window");
                                    }
                                    if let Err(error) = set_overlay_window_visible(window, visible)
                                    {
                                        debug!(%error, "could not change overlay visibility");
                                    }
                                    view.update(app, |view, cx| {
                                        view.model = model;
                                        view.visible = visible;
                                        view.suppressed_until_new_request = suppression;
                                        view.description_scroll_started = Instant::now();
                                        view.placement = placement;
                                        cx.notify();
                                    });
                                }
                                ServerMessage::Hidden { session_id }
                                    if hidden_message_matches(
                                        &view.read(app).model,
                                        &session_id,
                                    ) =>
                                {
                                    hide_detail_window(view.read(app).detail_window, app);
                                    if let Err(error) = set_overlay_window_visible(window, false) {
                                        debug!(%error, "could not hide overlay window");
                                    }
                                    view.update(app, |view, cx| {
                                        view.visible = false;
                                        cx.notify();
                                    });
                                    animate_description = false;
                                }
                                _ => {}
                            }
                        }
                        if animate_description {
                            view.update(app, |view, cx| {
                                if view.visible {
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
}

fn overlay_height(model: &RenderModel) -> f32 {
    let rows = model.candidates.len().clamp(1, MAX_VISIBLE_CANDIDATES) as f32;
    rows * 32.0
        + if model.ghost_text.is_some() {
            30.0
        } else {
            0.0
        }
}

fn maximum_overlay_height(model: &RenderModel) -> f32 {
    model
        .candidates
        .iter()
        .map(candidate_detail_height)
        .fold(overlay_height(model), f32::max)
}

fn visible_candidate_range(candidate_count: usize, selected: usize) -> std::ops::Range<usize> {
    let visible_count = candidate_count.min(MAX_VISIBLE_CANDIDATES);
    let selected = selected.min(candidate_count.saturating_sub(1));
    let start = selected
        .saturating_add(1)
        .saturating_sub(visible_count)
        .min(candidate_count.saturating_sub(visible_count));
    start..start + visible_count
}

fn display_candidate_label(label: &str, kind: CandidateKind) -> String {
    if matches!(kind, CandidateKind::File | CandidateKind::Directory) {
        truncate_middle(label, MAX_PATH_LABEL_WIDTH)
    } else {
        label.to_owned()
    }
}

fn candidate_kind_icon(kind: CandidateKind) -> &'static str {
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

fn approximate_text_width(value: &str) -> f32 {
    UnicodeWidthStr::width(value) as f32 * APPROX_TEXT_COLUMN_WIDTH
}

fn display_candidate_description(description: &str) -> String {
    description.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn adaptive_label_width(label: &str) -> f32 {
    (approximate_text_width(label) + 8.0).clamp(MIN_LABEL_WIDTH, MAX_LABEL_WIDTH)
}

fn description_viewport_width(label: &str) -> f32 {
    ROW_CONTENT_WIDTH - adaptive_label_width(label)
}

fn description_overflows(label: &str, description: &str) -> bool {
    approximate_text_width(description) > description_viewport_width(label)
}

fn description_scroll_offset(description: &str, viewport_width: f32, elapsed: Duration) -> f32 {
    let distance = (approximate_text_width(description) - viewport_width).max(0.0);
    if distance == 0.0 || elapsed < DESCRIPTION_SCROLL_PAUSE {
        return 0.0;
    }
    let travel = Duration::from_secs_f32(distance / DESCRIPTION_SCROLL_SPEED);
    let cycle = DESCRIPTION_SCROLL_PAUSE + travel + DESCRIPTION_SCROLL_END_PAUSE;
    let cycle_elapsed = elapsed.as_secs_f32() % cycle.as_secs_f32();
    let moving_at = cycle_elapsed - DESCRIPTION_SCROLL_PAUSE.as_secs_f32();
    if moving_at <= 0.0 {
        0.0
    } else {
        (moving_at * DESCRIPTION_SCROLL_SPEED).min(distance)
    }
}

fn selected_description_overflows(model: &RenderModel) -> bool {
    model
        .candidates
        .get(model.selected)
        .and_then(|candidate| {
            candidate.description.as_deref().map(|description| {
                let label = display_candidate_label(&candidate.label, candidate.kind);
                description_overflows(&label, &display_candidate_description(description))
            })
        })
        .unwrap_or(false)
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

fn model_has_content(model: &RenderModel) -> bool {
    !model.candidates.is_empty() || model.ghost_text.is_some()
}

fn model_is_suppressed(model: &RenderModel, suppression: Option<&(String, u64)>) -> bool {
    suppression.is_some_and(|(session_id, request_id)| {
        model.session_id == *session_id && model.request_id <= *request_id
    })
}

fn hidden_message_matches(model: &RenderModel, session_id: &str) -> bool {
    model.session_id == session_id
}

fn overlay_origin(model: &RenderModel, _height: f32) -> gpui::Point<gpui::Pixels> {
    model.anchor.map_or(point(px(120.0), px(120.0)), |anchor| {
        point(px(anchor.position.x), px(anchor.position.y + CURSOR_GAP))
    })
}

#[cfg(test)]
fn preferred_overlay_top(anchor: CursorAnchor, height: f32, screen_height: f32) -> f32 {
    let below = anchor.position.y + CURSOR_GAP;
    match preferred_overlay_placement(anchor, height, screen_height) {
        OverlayPlacement::Below => below,
        OverlayPlacement::Above => {
            (anchor.position.y - anchor.line_height - CURSOR_GAP - height).max(0.0)
        }
    }
}

fn preferred_overlay_placement(
    anchor: CursorAnchor,
    required_height: f32,
    screen_height: f32,
) -> OverlayPlacement {
    let below = anchor.position.y + CURSOR_GAP;
    let below_available = (screen_height - below).max(0.0);
    let above_available = (anchor.position.y - anchor.line_height - CURSOR_GAP).max(0.0);
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

fn overlay_top_for_placement(
    anchor: CursorAnchor,
    height: f32,
    placement: OverlayPlacement,
) -> f32 {
    match placement {
        OverlayPlacement::Below => anchor.position.y + CURSOR_GAP,
        OverlayPlacement::Above => {
            (anchor.position.y - anchor.line_height - CURSOR_GAP - height).max(0.0)
        }
    }
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn preferred_model_placement(model: &RenderModel) -> OverlayPlacement {
    use cocoa::{appkit::NSScreen, base::nil, foundation::NSRect};

    let Some(anchor) = model.anchor else {
        return OverlayPlacement::Below;
    };
    let screen = unsafe { NSScreen::mainScreen(nil) };
    let screen_frame: NSRect = unsafe { NSScreen::frame(screen) };
    preferred_overlay_placement(
        anchor,
        maximum_overlay_height(model),
        screen_frame.size.height as f32,
    )
}

#[cfg(not(target_os = "macos"))]
fn preferred_model_placement(_model: &RenderModel) -> OverlayPlacement {
    OverlayPlacement::Below
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn placement_height_limit(model: &RenderModel, placement: OverlayPlacement) -> f32 {
    use cocoa::{appkit::NSScreen, base::nil, foundation::NSRect};

    let Some(anchor) = model.anchor else {
        return f32::MAX;
    };
    let screen = unsafe { NSScreen::mainScreen(nil) };
    let screen_frame: NSRect = unsafe { NSScreen::frame(screen) };
    match placement {
        OverlayPlacement::Below => {
            (screen_frame.size.height as f32 - anchor.position.y - CURSOR_GAP).max(0.0)
        }
        OverlayPlacement::Above => (anchor.position.y - anchor.line_height - CURSOR_GAP).max(0.0),
    }
}

#[cfg(not(target_os = "macos"))]
fn placement_height_limit(_model: &RenderModel, _placement: OverlayPlacement) -> f32 {
    f32::MAX
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn reposition_window(
    window: &Window,
    model: &RenderModel,
    height: f32,
    placement: OverlayPlacement,
) -> anyhow::Result<()> {
    use cocoa::{
        appkit::NSScreen,
        base::nil,
        foundation::{NSPoint, NSRect},
    };
    use objc::{msg_send, runtime::Object, sel, sel_impl};

    let Some(anchor) = model.anchor else {
        return Ok(());
    };
    let native_window = native_window(window)?;
    let screen = unsafe { NSScreen::mainScreen(nil) };
    let screen_frame: NSRect = unsafe { NSScreen::frame(screen) };
    let top = overlay_top_for_placement(anchor, height, placement);
    let origin_x = f64::from(anchor.position.x);
    let origin_y =
        screen_frame.origin.y + screen_frame.size.height - f64::from(top) - f64::from(height);
    // Moving NSWindow synchronously from inside GPUI's update callback re-enters
    // GPUI while its window state is borrowed. Dispatching to the next main-queue
    // turn keeps the native move ordered without triggering that re-entrant borrow.
    dispatch::Queue::main().exec_async(move || {
        let native_window = native_window as *mut Object;
        let origin = NSPoint::new(origin_x, origin_y);
        unsafe {
            let _: () = msg_send![native_window, setFrameOrigin: origin];
        }
    });
    Ok(())
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn reposition_detail_window(
    window: &Window,
    model: &RenderModel,
    height: f32,
    placement: OverlayPlacement,
) -> anyhow::Result<()> {
    use cocoa::{
        appkit::NSScreen,
        base::nil,
        foundation::{NSPoint, NSRect},
    };
    use objc::{msg_send, runtime::Object, sel, sel_impl};

    let Some(anchor) = model.anchor else {
        return Ok(());
    };
    let native_window = native_window(window)?;
    let screen = unsafe { NSScreen::mainScreen(nil) };
    let screen_frame: NSRect = unsafe { NSScreen::frame(screen) };
    let right = anchor.position.x + OVERLAY_WIDTH + DETAIL_WINDOW_GAP;
    let x = if right + DETAIL_WIDTH <= screen_frame.size.width as f32 {
        right
    } else {
        (anchor.position.x - DETAIL_WINDOW_GAP - DETAIL_WIDTH).max(0.0)
    };
    let top = overlay_top_for_placement(anchor, height, placement);
    let origin_y =
        screen_frame.origin.y + screen_frame.size.height - f64::from(top) - f64::from(height);
    dispatch::Queue::main().exec_async(move || {
        let native_window = native_window as *mut Object;
        let origin = NSPoint::new(f64::from(x), origin_y);
        unsafe {
            let _: () = msg_send![native_window, setFrameOrigin: origin];
        }
    });
    Ok(())
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn set_overlay_window_visible(window: &Window, visible: bool) -> anyhow::Result<()> {
    use cocoa::base::nil;
    use objc::{
        msg_send,
        runtime::{NO, Object, YES},
        sel, sel_impl,
    };

    let native_window = native_window(window)?;
    // AppKit visibility changes can re-enter GPUI, so keep them ordered on the
    // next main-queue turn just like native window repositioning.
    dispatch::Queue::main().exec_async(move || {
        let native_window = native_window as *mut Object;
        unsafe {
            let _: () = msg_send![native_window, setIgnoresMouseEvents: NO];
            let _: () = msg_send![native_window, setAcceptsMouseMovedEvents: YES];
            let _: () = msg_send![native_window, setBecomesKeyOnlyIfNeeded: YES];
            if visible {
                let _: () = msg_send![native_window, orderFront: nil];
            } else {
                let _: () = msg_send![native_window, orderOut: nil];
            }
        }
    });
    Ok(())
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn set_detail_window_visible(window: &Window, visible: bool) -> anyhow::Result<()> {
    use cocoa::base::nil;
    use objc::{
        msg_send,
        runtime::{Object, YES},
        sel, sel_impl,
    };

    let native_window = native_window(window)?;
    dispatch::Queue::main().exec_async(move || {
        let native_window = native_window as *mut Object;
        unsafe {
            let _: () = msg_send![native_window, setIgnoresMouseEvents: YES];
            let _: () = msg_send![native_window, setBecomesKeyOnlyIfNeeded: YES];
            if visible {
                let _: () = msg_send![native_window, orderFront: nil];
            } else {
                let _: () = msg_send![native_window, orderOut: nil];
            }
        }
    });
    Ok(())
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn alacritty_is_frontmost() -> bool {
    use std::ffi::CStr;

    use objc::{class, msg_send, runtime::Object, sel, sel_impl};

    if let Ok(value) = std::env::var("WISP_ALACRITTY_ACTIVE") {
        return matches!(value.as_str(), "1" | "true" | "yes");
    }
    unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        let application: *mut Object = msg_send![workspace, frontmostApplication];
        if application.is_null() {
            return false;
        }
        let name: *mut Object = msg_send![application, localizedName];
        if name.is_null() {
            return false;
        }
        let bytes: *const std::os::raw::c_char = msg_send![name, UTF8String];
        !bytes.is_null()
            && CStr::from_ptr(bytes)
                .to_string_lossy()
                .eq_ignore_ascii_case("Alacritty")
    }
}

#[cfg(not(target_os = "macos"))]
fn alacritty_is_frontmost() -> bool {
    true
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn native_window(window: &Window) -> anyhow::Result<usize> {
    use objc::{msg_send, runtime::Object, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow::anyhow!("read GPUI window handle: {error:?}"))?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err(anyhow::anyhow!("GPUI window is not backed by AppKit"));
    };
    let view = handle.ns_view.as_ptr().cast::<Object>();
    let native_window: *mut Object = unsafe { msg_send![view, window] };
    if native_window.is_null() {
        return Err(anyhow::anyhow!("GPUI AppKit view has no window"));
    }
    Ok(native_window as usize)
}

#[cfg(not(target_os = "macos"))]
fn reposition_window(
    _window: &Window,
    _model: &RenderModel,
    _height: f32,
    _placement: OverlayPlacement,
) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn reposition_detail_window(
    _window: &Window,
    _model: &RenderModel,
    _height: f32,
    _placement: OverlayPlacement,
) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn set_overlay_window_visible(_window: &Window, _visible: bool) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn set_detail_window_visible(_window: &Window, _visible: bool) -> anyhow::Result<()> {
    Ok(())
}

fn spawn_subscription(socket: PathBuf, sender: mpsc::Sender<ServerMessage>) {
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build overlay IPC runtime");
        runtime.block_on(async move {
            loop {
                match subscribe_once(&socket, &sender).await {
                    Ok(()) => return,
                    Err(error) => {
                        debug!(%error, "overlay subscription reconnecting");
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                }
            }
        });
    });
}

fn spawn_interaction_worker(socket: PathBuf, receiver: mpsc::Receiver<OverlayAction>) {
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build overlay interaction runtime");
        while let Ok(action) = receiver.recv() {
            let message = match action {
                OverlayAction::Select { session_id, index } => {
                    ClientMessage::SelectCandidate { session_id, index }
                }
            };
            match runtime.block_on(overlay_request(&socket, message)) {
                Ok(ServerMessage::Error { message }) => {
                    debug!(%message, "overlay interaction was rejected");
                }
                Ok(_) => {}
                Err(error) => debug!(%error, "overlay interaction failed"),
            }
        }
    });
}

async fn overlay_request(socket: &Path, message: ClientMessage) -> anyhow::Result<ServerMessage> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect to daemon at {}", socket.display()))?;
    let mut connection = framed(stream);
    send_message(&mut connection, &message).await?;
    receive_message(&mut connection)
        .await?
        .context("daemon closed overlay interaction without a response")
}

async fn subscribe_once(
    socket: &PathBuf,
    sender: &mpsc::Sender<ServerMessage>,
) -> anyhow::Result<()> {
    let stream = UnixStream::connect(socket).await?;
    let mut connection = framed(stream);
    send_message(&mut connection, &ClientMessage::SubscribeOverlay).await?;
    while let Some(message) = receive_message(&mut connection).await? {
        if sender.send(message).is_err() {
            return Ok(());
        }
    }
    Err(anyhow::anyhow!("daemon closed overlay subscription"))
}

fn default_socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("WISP_SOCKET") {
        return path.into();
    }
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
        || std::env::temp_dir().join(format!("wisp-{}.sock", socket_identity())),
        |directory| PathBuf::from(directory).join("wisp.sock"),
    )
}

fn socket_identity() -> String {
    std::env::var("USER")
        .unwrap_or_else(|_| "user".into())
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_model(session_id: &str) -> RenderModel {
        RenderModel {
            request_id: 1,
            session_id: session_id.into(),
            anchor: None,
            candidates: Vec::new(),
            selected: 0,
            ghost_text: None,
        }
    }

    #[test]
    fn empty_model_is_not_visible() {
        assert!(!model_has_content(&empty_model("current")));
    }

    #[test]
    fn hidden_message_only_applies_to_current_session() {
        let model = empty_model("current");
        assert!(hidden_message_matches(&model, "current"));
        assert!(!hidden_message_matches(&model, "stale"));
    }

    #[test]
    fn blurred_request_stays_suppressed_until_a_new_request() {
        let mut model = empty_model("current");
        model.request_id = 7;
        let suppression = Some(("current".to_owned(), 7));

        assert!(model_is_suppressed(&model, suppression.as_ref()));
        model.request_id = 6;
        assert!(model_is_suppressed(&model, suppression.as_ref()));
        model.request_id = 8;
        assert!(!model_is_suppressed(&model, suppression.as_ref()));
        model.session_id = "other".into();
        model.request_id = 7;
        assert!(!model_is_suppressed(&model, suppression.as_ref()));
    }

    #[test]
    fn overlay_has_padding_below_cursor() {
        let mut model = empty_model("current");
        model.anchor = Some(wisp_protocol::CursorAnchor {
            position: wisp_protocol::ScreenPoint { x: 320.0, y: 480.0 },
            line_height: 20.0,
            cell_width: 10.0,
        });
        assert_eq!(overlay_origin(&model, 160.0), point(px(320.0), px(488.0)));
    }

    #[test]
    fn candidate_viewport_follows_selection() {
        assert_eq!(visible_candidate_range(0, 0), 0..0);
        assert_eq!(visible_candidate_range(3, 2), 0..3);
        assert_eq!(visible_candidate_range(12, 0), 0..8);
        assert_eq!(visible_candidate_range(12, 7), 0..8);
        assert_eq!(visible_candidate_range(12, 8), 1..9);
        assert_eq!(visible_candidate_range(12, 11), 4..12);
    }

    #[test]
    fn mouse_scroll_moves_selection_without_wrapping() {
        assert_eq!(candidate_index_after_scroll(0, 12, -1.0, 1), 1);
        assert_eq!(candidate_index_after_scroll(7, 12, -1.0, 3), 10);
        assert_eq!(candidate_index_after_scroll(11, 12, -1.0, 4), 11);
        assert_eq!(candidate_index_after_scroll(4, 12, 1.0, 2), 2);
        assert_eq!(candidate_index_after_scroll(0, 12, 1.0, 2), 0);
    }

    #[test]
    fn long_candidate_details_expand_the_native_window() {
        let short = Candidate {
            label: "git".into(),
            insert_text: "git".into(),
            description: Some("version control".into()),
            kind: CandidateKind::Command,
            score: 1.0,
            replace_start: 0,
            replace_end: 3,
        };
        let mut long = short.clone();
        long.description = Some("a detailed explanation ".repeat(100));
        assert!(candidate_detail_height(&long) > candidate_detail_height(&short));
        assert_eq!(candidate_detail_height(&long), 560.0);
    }

    #[test]
    fn long_path_labels_are_shortened_without_changing_other_candidates() {
        let path = "workspace/a-very-long-directory-name/another-directory/source-file.rs";
        let shortened = display_candidate_label(path, CandidateKind::File);

        assert!(shortened.contains('…'));
        assert!(shortened.ends_with("source-file.rs"));
        assert!(UnicodeWidthStr::width(shortened.as_str()) <= MAX_PATH_LABEL_WIDTH);
        assert_eq!(display_candidate_label(path, CandidateKind::Command), path);
        assert_eq!(
            display_candidate_label("目录/非常非常非常长的文件名称.rs", CandidateKind::File),
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
        assert!(
            description_viewport_width("git")
                > description_viewport_width("an-extremely-long-command-name-that-needs-clipping")
        );
        assert!(description_viewport_width("git") > 300.0);
    }

    #[test]
    fn selected_long_description_scrolls_after_a_pause() {
        let description = "A very long completion description that cannot fit in its viewport";
        let width = 120.0;
        assert_eq!(
            description_scroll_offset(description, width, Duration::ZERO),
            0.0
        );
        assert!(description_scroll_offset(description, width, Duration::from_secs(2)) > 0.0);
    }

    #[test]
    fn description_control_whitespace_is_collapsed_to_one_line() {
        assert_eq!(
            display_candidate_description("first line\r\nsecond\tline   tail"),
            "first line second line tail"
        );
        assert_eq!(display_candidate_description("\n\r\t"), "");
    }

    #[test]
    fn overlay_flips_above_cursor_when_below_does_not_fit() {
        let anchor = CursorAnchor {
            position: wisp_protocol::ScreenPoint { x: 320.0, y: 480.0 },
            line_height: 20.0,
            cell_width: 10.0,
        };

        assert_eq!(preferred_overlay_top(anchor, 160.0, 1000.0), 488.0);
        assert_eq!(preferred_overlay_top(anchor, 160.0, 600.0), 292.0);
    }

    #[test]
    fn locked_detail_placement_never_crosses_the_cursor_line() {
        let anchor = CursorAnchor {
            position: wisp_protocol::ScreenPoint { x: 320.0, y: 480.0 },
            line_height: 20.0,
            cell_width: 10.0,
        };

        let below_top = overlay_top_for_placement(anchor, 320.0, OverlayPlacement::Below);
        assert!(below_top >= anchor.position.y + CURSOR_GAP);

        let above_top = overlay_top_for_placement(anchor, 320.0, OverlayPlacement::Above);
        assert!(above_top + 320.0 <= anchor.position.y - anchor.line_height - CURSOR_GAP);
    }

    #[test]
    fn detail_height_is_used_before_choosing_a_side() {
        let anchor = CursorAnchor {
            position: wisp_protocol::ScreenPoint { x: 320.0, y: 500.0 },
            line_height: 20.0,
            cell_width: 10.0,
        };
        assert_eq!(
            preferred_overlay_placement(anchor, 420.0, 700.0),
            OverlayPlacement::Above
        );
    }
}
