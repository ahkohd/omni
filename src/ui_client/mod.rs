mod anim;
mod aurora;
mod config;
mod ipc;
mod lock;
mod model;
mod platform;
mod position;
mod reducer;
mod view;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use clap::Parser;

use anim::{AnimationConfig, TickAction, normalized_audio_energy, tick};
use aurora::{
    AURORA_FLOW_SPEED_BOOST, AURORA_IDLE_FLOW_SPEED, AURORA_PEAK_GAIN, AURORA_RMS_GAIN,
    AuroraRenderer, AuroraUniformInput,
};
use config::{
    Cli, PILL_TRANSCRIPT_BASE_HEIGHT, PILL_TRANSCRIPT_MAX_HEIGHT, PILL_TRANSCRIPT_MIN_HEIGHT,
    PILL_WAVE_HEIGHT, SAFE_BOTTOM_MARGIN, TRANSCRIPT_CHARS_PER_LINE_ESTIMATE,
    TRANSCRIPT_LINE_HEIGHT, TRANSCRIPT_MAX_LINES, UiRuntimeConfig, WINDOW_HEIGHT, WINDOW_WIDTH,
};
use gpui::{
    App, Context as UiContext, Pixels, Render, Size, Window, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions, div, prelude::*, px, size,
};
use gpui_platform::application;
use ipc::spawn_reader;
use lock::acquire_instance_lock;
use model::{InboundMessage, UiState};
use position::{bounds_from_normalized, contains, load_position, normalize_center, save_position};
use reducer::apply_message;
use view::{bottom_center_bounds, render_transcript_layout};

struct TranscribeUi {
    rx: mpsc::Receiver<InboundMessage>,
    cfg: UiRuntimeConfig,
    state: UiState,
    aurora: AuroraRenderer,
    #[cfg(target_os = "macos")]
    native_window_visible: bool,
}

impl TranscribeUi {
    fn new(rx: mpsc::Receiver<InboundMessage>, cfg: UiRuntimeConfig) -> Self {
        let now = Instant::now();

        Self {
            rx,
            state: UiState::new(now, cfg.idle_linger, PILL_WAVE_HEIGHT),
            cfg,
            aurora: AuroraRenderer::new(),
            #[cfg(target_os = "macos")]
            native_window_visible: false,
        }
    }

    fn animation_config(&self) -> AnimationConfig {
        AnimationConfig {
            idle_linger: self.cfg.idle_linger,
            transcript_chars_per_line_estimate: TRANSCRIPT_CHARS_PER_LINE_ESTIMATE,
            transcript_max_lines: TRANSCRIPT_MAX_LINES,
            pill_wave_height: PILL_WAVE_HEIGHT,
            pill_transcript_min_height: PILL_TRANSCRIPT_MIN_HEIGHT,
            pill_transcript_max_height: PILL_TRANSCRIPT_MAX_HEIGHT,
            pill_transcript_base_height: PILL_TRANSCRIPT_BASE_HEIGHT,
            transcript_line_height: TRANSCRIPT_LINE_HEIGHT,
            aurora_idle_flow_speed: AURORA_IDLE_FLOW_SPEED,
            aurora_flow_speed_boost: AURORA_FLOW_SPEED_BOOST,
            aurora_rms_gain: AURORA_RMS_GAIN,
            aurora_peak_gain: AURORA_PEAK_GAIN,
        }
    }

    fn poll_inbound(&mut self, now: Instant) {
        while let Ok(message) = self.rx.try_recv() {
            apply_message(&mut self.state, message, now, self.cfg.hide_linger);
        }
    }

    fn normalized_audio_energy(&self) -> f32 {
        normalized_audio_energy(&self.state, self.animation_config())
    }

    fn tick_state(&mut self, now: Instant, window: &mut Window) {
        let cfg = self.animation_config();
        if tick(&mut self.state, now, cfg) == TickAction::RemoveWindow {
            window.remove_window();
            return;
        }

        if self.state.transcript.should_scroll_to_bottom {
            self.state.transcript.scroll.scroll_to_bottom();
            self.state.transcript.should_scroll_to_bottom = false;
        }
    }

    fn render_aurora_overlay(&self) -> gpui::Div {
        self.aurora.render_overlay(AuroraUniformInput {
            time: self.state.aurora.time,
            energy: self.normalized_audio_energy(),
            silent: self.state.audio.silent,
            fill_state: self.state.transcript.fill_state,
        })
    }

    #[cfg(target_os = "macos")]
    fn sync_native_window_visibility(&mut self, window: &mut Window) {
        let target_visible = self.state.visibility.visible;
        if self.native_window_visible == target_visible {
            return;
        }

        platform::macos::sync_window_visibility(window, target_visible);
        self.native_window_visible = target_visible;
    }

    #[cfg(not(target_os = "macos"))]
    fn sync_native_window_visibility(&mut self, _window: &mut Window) {}
}

impl Render for TranscribeUi {
    fn render(&mut self, window: &mut Window, _cx: &mut UiContext<Self>) -> impl IntoElement {
        self.aurora.ensure_resources(window);

        let mut root = div().id("transcribe-ui-root").size_full();

        if self.state.visibility.panel_opacity <= 0.01
            && !self.state.visibility.visible
            && self.state.visibility.hide_deadline.is_none()
        {
            return root;
        }

        let content = render_transcript_layout(&self.state);

        let shell = div()
            .relative()
            .size_full()
            .overflow_hidden()
            .child(self.render_aurora_overlay())
            .child(
                div()
                    .relative()
                    .size_full()
                    .px(px(12.0))
                    .py(px(14.0))
                    .child(content),
            );

        root = root.child(shell);
        root
    }
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let runtime_cfg = UiRuntimeConfig {
        ui_socket: resolve_ui_socket_path(cli.socket.as_deref())?,
        daemon_socket: resolve_daemon_socket_path(cli.daemon_socket.as_deref())?,
        idle_linger: Duration::from_millis(cli.idle_ms.max(200)),
        hide_linger: Duration::from_millis(cli.hide_ms.max(50)),
        reconnect_delay: Duration::from_millis(cli.retry_ms.max(20)),
    };

    let lock_runtime_dir = runtime_cfg
        .ui_socket
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(resolve_runtime_dir()?);

    let _instance_lock = match acquire_instance_lock(&lock_runtime_dir)? {
        Some(lock) => lock,
        None => return Ok(()),
    };

    let (tx, rx) = mpsc::channel::<InboundMessage>();
    spawn_reader(runtime_cfg.clone(), tx);

    let position_store_dir = resolve_ui_position_dir()?;
    let saved_position = load_position(&position_store_dir);

    application().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        #[cfg(target_os = "macos")]
        {
            let _ = platform::macos::set_activation_policy_accessory();
        }

        let Some(display) = cx
            .primary_display()
            .or_else(|| cx.displays().into_iter().next())
        else {
            cx.quit();
            return;
        };

        let visible_bounds = display.visible_bounds();
        let window_size: Size<Pixels> = size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT));
        let bounds = if let Some(saved) = saved_position {
            bounds_from_normalized(visible_bounds, window_size, saved)
        } else {
            bottom_center_bounds(visible_bounds, window_size, SAFE_BOTTOM_MARGIN)
        };

        let mut rx_slot = Some(rx);
        let cfg_for_view = runtime_cfg.clone();
        let position_store_dir_for_window = position_store_dir.clone();
        let show_window_on_create = !cfg!(target_os = "macos");
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            display_id: Some(display.id()),
            titlebar: None,
            focus: false,
            show: show_window_on_create,
            kind: WindowKind::PopUp,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };

        #[cfg(target_os = "macos")]
        let original_activation_policy = {
            let original = platform::macos::current_activation_policy();
            let _ = platform::macos::set_activation_policy_prohibited();
            original
        };

        let open_result = cx.open_window(options, move |window, cx| {
            #[cfg(target_os = "macos")]
            {
                platform::macos::install_backdrop(window);
            }

            let receiver = rx_slot.take().expect("ui receiver should be available");
            let cfg = cfg_for_view.clone();
            let position_dir_for_observer = position_store_dir_for_window.clone();

            cx.new(move |cx| {
                cx.observe_window_bounds(window, move |_, window, cx| {
                    let window_bounds = window.bounds();
                    let center = window_bounds.center();

                    let visible = cx
                        .displays()
                        .into_iter()
                        .map(|display| display.visible_bounds())
                        .find(|bounds| contains(*bounds, center))
                        .or_else(|| cx.primary_display().map(|display| display.visible_bounds()))
                        .unwrap_or(window_bounds);

                    if let Some(normalized) = normalize_center(window_bounds, visible) {
                        save_position(&position_dir_for_observer, normalized);
                    }
                })
                .detach();

                TranscribeUi::new(receiver, cfg)
            })
        });

        #[cfg(target_os = "macos")]
        {
            if let Some(policy) = original_activation_policy {
                let _ = platform::macos::set_activation_policy(policy);
            } else {
                let _ = platform::macos::set_activation_policy_accessory();
            }
        }

        let Ok(window_handle) = open_result else {
            cx.quit();
            return;
        };

        let driver_window_handle = window_handle;
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;

                let now = Instant::now();
                let updated = driver_window_handle
                    .update(cx, |ui, window, _cx| {
                        ui.poll_inbound(now);
                        ui.tick_state(now, window);
                        ui.sync_native_window_visibility(window);
                        window.refresh();
                    })
                    .is_ok();

                if !updated {
                    break;
                }
            }
        })
        .detach();

        cx.activate(false);
    });

    Ok(())
}

fn resolve_ui_socket_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    Ok(resolve_runtime_dir()?.join("ui.sock"))
}

fn resolve_daemon_socket_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    Ok(resolve_runtime_dir()?.join("daemon.sock"))
}

fn resolve_ui_position_dir() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("OMNI_UI_POSITION_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    if let Some(data_dir) = dirs::data_local_dir() {
        return Ok(data_dir.join("omni"));
    }

    let home = dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve home directory"))?;
    Ok(home.join(".local").join("share").join("omni"))
}

fn resolve_runtime_dir() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("OMNI_RUNTIME_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    if let Ok(xdg_runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let trimmed = xdg_runtime_dir.trim();
        if !trimmed.is_empty() {
            return Ok(Path::new(trimmed).join("omni"));
        }
    }

    let home = dirs::home_dir().ok_or_else(|| anyhow!("failed to resolve home directory"))?;
    Ok(home.join(".local").join("state").join("omni"))
}
