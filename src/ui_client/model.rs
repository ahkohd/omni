use std::time::{Duration, Instant};

use gpui::ScrollHandle;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct UiEventEnvelope {
    #[allow(dead_code)]
    pub v: u8,
    #[allow(dead_code)]
    pub seq: u64,
    #[allow(dead_code)]
    pub at_ms: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct DaemonSnapshot {
    pub running: bool,
    pub recording: bool,
    pub transcript_preview: Option<String>,
}

#[derive(Debug)]
pub enum InboundMessage {
    UiEvent(UiEventEnvelope),
    Snapshot(DaemonSnapshot),
}

#[derive(Debug)]
pub struct VisibilityState {
    pub visible: bool,
    pub panel_opacity: f32,
    pub hide_deadline: Option<Instant>,
    pub idle_deadline: Option<Instant>,
}

#[derive(Debug)]
pub struct TranscriptState {
    pub text: String,
    pub bold_from: usize,
    pub text_pulse: f32,
    pub fill_state: f32,
    pub shell_height: f32,
    pub scroll: ScrollHandle,
    pub should_scroll_to_bottom: bool,
}

#[derive(Debug)]
pub struct AudioState {
    pub silent: bool,
    pub rms: f32,
    pub peak: f32,
    pub energy_display: f32,
    pub peak_display: f32,
    pub wave_phase: f32,
}

#[derive(Debug)]
pub struct AuroraState {
    pub time: f32,
}

#[derive(Debug)]
pub struct UiState {
    pub visibility: VisibilityState,
    pub transcript: TranscriptState,
    pub audio: AudioState,
    pub aurora: AuroraState,
    pub accept_transcript_events: bool,
    pub last_frame: Instant,
}

impl UiState {
    pub fn new(now: Instant, idle_linger: Duration, initial_shell_height: f32) -> Self {
        Self {
            visibility: VisibilityState {
                visible: false,
                panel_opacity: 0.0,
                hide_deadline: None,
                idle_deadline: Some(now + idle_linger),
            },
            transcript: TranscriptState {
                text: String::new(),
                bold_from: 0,
                text_pulse: 1.0,
                fill_state: 0.0,
                shell_height: initial_shell_height,
                scroll: ScrollHandle::new(),
                should_scroll_to_bottom: false,
            },
            audio: AudioState {
                silent: true,
                rms: 0.0,
                peak: 0.0,
                energy_display: 0.0,
                peak_display: 0.0,
                wave_phase: 0.0,
            },
            aurora: AuroraState { time: 0.0 },
            accept_transcript_events: true,
            last_frame: now,
        }
    }
}
