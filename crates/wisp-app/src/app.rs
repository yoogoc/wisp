use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use clap::Parser;
use gpui::{
    App, AppContext, Application, Bounds, Entity, Timer, Window, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions, px, size,
};
use tracing::debug;
use tracing_subscriber::EnvFilter;
use wisp_config::{OverlayConfig, WispConfig};
use wisp_protocol::{RenderModel, ServerMessage};

use crate::{
    ipc::{default_socket_path, spawn_interaction_worker, spawn_subscription},
    layout::{overlay_height, overlay_origin},
    platform::{
        configure_application, install_status_item, preferred_model_placement,
        reposition_overlay_window, set_detail_window_visible, set_overlay_window_visible,
        terminal_is_frontmost,
    },
    presentation::selected_description_overflows,
    state::OverlayState,
    view::{CandidateDetailView, OverlayView, hide_detail_window},
};

#[derive(Debug, Parser)]
#[command(version, about = "Wisp GPUI autocomplete application")]
struct Args {
    #[arg(long, env = "WISP_SOCKET")]
    socket: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
}

pub fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "wisp=info".into()))
        .init();
    let args = Args::parse();
    let socket = args.socket.unwrap_or_else(default_socket_path);
    wisp_daemon::ensure_no_running_instance(&socket)?;
    let config_path = args.config.unwrap_or_else(wisp_daemon::default_config_path);
    let loaded_config = WispConfig::load(&config_path)?;
    let config = Arc::new(loaded_config.overlay);
    let (sender, receiver) = mpsc::channel();
    let (interaction_sender, interaction_receiver) = mpsc::channel();
    spawn_daemon(socket.clone(), config_path);
    spawn_subscription(socket.clone(), sender, config.reconnect_ms);
    spawn_interaction_worker(socket, interaction_receiver);

    let first = empty_render_model();
    let first_placement = preferred_model_placement(&config, &first);
    let receiver = Arc::new(Mutex::new(receiver));

    Application::new().run(move |cx: &mut App| {
        if let Err(error) = configure_application() {
            debug!(%error, "could not configure overlay application policy");
        }
        if let Err(error) = install_status_item() {
            debug!(%error, "could not install Wisp status item");
        }
        let terminal_active = terminal_is_frontmost(first.terminal_application_id.as_deref());
        let height = overlay_height(&config, &first);
        let anchor = overlay_origin(&config, &first);
        let bounds = Bounds {
            origin: anchor,
            size: size(px(config.width), px(height)),
        };
        let detail_bounds = Bounds {
            origin: anchor,
            size: size(px(config.detail_width), px(config.detail_min_height)),
        };
        let detail_window = cx
            .open_window(popup_options(detail_bounds), |window, cx| {
                if let Err(error) = set_detail_window_visible(window, false) {
                    debug!(%error, "could not configure completion detail window");
                }
                cx.new(|_| CandidateDetailView::empty())
            })
            .expect("open Wisp completion detail window");
        let receiver = Arc::clone(&receiver);
        let view_config = Arc::clone(&config);
        let poll_config = Arc::clone(&config);
        cx.open_window(popup_options(bounds), move |window, cx| {
            let state = OverlayState::new(first, terminal_active, first_placement);
            let initially_visible = state.visible;
            let view = cx.new(|_| OverlayView {
                state,
                config: view_config,
                detail_window,
                interaction_sender,
            });
            let initial_model = view.read(cx).state.model.clone();
            if let Err(error) = reposition_overlay_window(
                window,
                &poll_config,
                &initial_model,
                overlay_height(&poll_config, &initial_model),
                first_placement,
            ) {
                debug!(%error, "could not position initial overlay window");
            }
            if let Err(error) = set_overlay_window_visible(window, initially_visible) {
                debug!(%error, "could not configure overlay window");
            }
            poll_messages(
                window,
                cx,
                view.clone(),
                receiver,
                poll_config,
                terminal_active,
            );
            view
        })
        .expect("open Wisp overlay window");
    });
    Ok(())
}

fn popup_options(bounds: Bounds<gpui::Pixels>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
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
    }
}

fn poll_messages(
    window: &mut Window,
    cx: &mut App,
    view: Entity<OverlayView>,
    receiver: Arc<Mutex<mpsc::Receiver<ServerMessage>>>,
    config: Arc<OverlayConfig>,
    initial_terminal_active: bool,
) {
    window
        .spawn(cx, async move |cx| {
            let mut terminal_active = initial_terminal_active;
            let mut last_activity_check = Instant::now();
            let mut animate_description = initial_terminal_active
                && cx
                    .update(|_, app| {
                        selected_description_overflows(&config, &view.read(app).state.model)
                    })
                    .unwrap_or(false);
            loop {
                Timer::after(Duration::from_millis(config.frame_interval_ms)).await;
                let activity_changed = if last_activity_check.elapsed()
                    >= Duration::from_millis(config.activity_check_ms)
                {
                    last_activity_check = Instant::now();
                    let application_id = cx
                        .update(|_, app| view.read(app).state.model.terminal_application_id.clone())
                        .ok()
                        .flatten();
                    let active = terminal_is_frontmost(application_id.as_deref());
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
                            let mut visible = false;
                            view.update(app, |view, cx| {
                                view.state.set_terminal_active(terminal_active);
                                visible = view.state.visible;
                                animate_description = visible
                                    && selected_description_overflows(&config, &view.state.model);
                                cx.notify();
                            });
                            if let Err(error) = set_overlay_window_visible(window, visible) {
                                debug!(%error, "could not follow terminal activation");
                            }
                        }
                        for message in messages {
                            match message {
                                ServerMessage::Render { model } => {
                                    terminal_active = terminal_is_frontmost(
                                        model.terminal_application_id.as_deref(),
                                    );
                                    hide_detail_window(view.read(app).detail_window, app);
                                    let placement = preferred_model_placement(&config, &model);
                                    let height = overlay_height(&config, &model);
                                    window.resize(size(px(config.width), px(height)));
                                    if let Err(error) = reposition_overlay_window(
                                        window, &config, &model, height, placement,
                                    ) {
                                        debug!(%error, "could not reposition overlay window");
                                    }
                                    let mut visible = false;
                                    view.update(app, |view, cx| {
                                        view.state.render(model, placement);
                                        view.state.set_terminal_active(terminal_active);
                                        visible = view.state.visible;
                                        animate_description = visible
                                            && selected_description_overflows(
                                                &config,
                                                &view.state.model,
                                            );
                                        cx.notify();
                                    });
                                    if let Err(error) = set_overlay_window_visible(window, visible)
                                    {
                                        debug!(%error, "could not change overlay visibility");
                                    }
                                }
                                ServerMessage::Hidden { session_id } => {
                                    let mut hidden = false;
                                    view.update(app, |view, cx| {
                                        hidden = view.state.hide(&session_id);
                                        if hidden {
                                            cx.notify();
                                        }
                                    });
                                    if hidden {
                                        hide_detail_window(view.read(app).detail_window, app);
                                        if let Err(error) =
                                            set_overlay_window_visible(window, false)
                                        {
                                            debug!(%error, "could not hide overlay window");
                                        }
                                        animate_description = false;
                                    }
                                }
                                _ => {}
                            }
                        }
                        if animate_description {
                            view.update(app, |view, cx| {
                                if view.state.visible {
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

fn spawn_daemon(socket: PathBuf, config: PathBuf) {
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build Wisp daemon runtime");
        let shutdown = tokio_util::sync::CancellationToken::new();
        if let Err(error) = runtime.block_on(wisp_daemon::run(socket, config, shutdown)) {
            debug!(%error, "Wisp daemon stopped");
        }
    });
}

fn empty_render_model() -> RenderModel {
    RenderModel {
        request_id: 0,
        session_id: String::new(),
        terminal_application_id: None,
        anchor: None,
        candidates: Vec::new(),
        selected: 0,
        ghost_text: None,
    }
}
