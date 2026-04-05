use std::time::{Duration, Instant};

use crate::ui_client::model::{DaemonSnapshot, InboundMessage, UiEventEnvelope, UiState};

pub fn apply_message(
    state: &mut UiState,
    message: InboundMessage,
    now: Instant,
    hide_linger: Duration,
) {
    match message {
        InboundMessage::Snapshot(snapshot) => apply_snapshot(state, snapshot, now),
        InboundMessage::UiEvent(event) => apply_event(state, event, now, hide_linger),
    }
}

pub fn apply_snapshot(state: &mut UiState, snapshot: DaemonSnapshot, now: Instant) {
    if !snapshot.running {
        return;
    }

    if snapshot.recording {
        show(state, now);
        if let Some(preview) = snapshot
            .transcript_preview
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            set_transcript(state, preview);
        }
    }
}

pub fn apply_event(
    state: &mut UiState,
    event: UiEventEnvelope,
    now: Instant,
    _hide_linger: Duration,
) {
    match event.event_type.as_str() {
        "ui.show" | "transcribe.started" => show(state, now),
        "ui.hide" | "transcribe.stopped" => {
            if let Some(transcript) = event
                .payload
                .get("transcript")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                set_transcript(state, transcript);
            }

            hide_immediately(state, now);
        }
        "audio.energy" => {
            state.audio.rms = event
                .payload
                .get("rms")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0) as f32;
            state.audio.peak = event
                .payload
                .get("peak")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0) as f32;
            state.audio.silent = event
                .payload
                .get("silent")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
        }
        "transcript.delta" => {
            if let Some(preview) = event
                .payload
                .get("preview")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                set_transcript(state, preview);
                show(state, now);
            } else if let Some(delta) = event
                .payload
                .get("delta")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
            {
                state.transcript.bold_from = state.transcript.text.len();
                state.transcript.text.push_str(delta);
                state.transcript.text_pulse = 0.0;
                state.transcript.should_scroll_to_bottom = true;
                show(state, now);
            }
        }
        "transcript.snapshot" => {
            if let Some(text) = event
                .payload
                .get("text")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                set_transcript(state, text);
                show(state, now);
            }
        }
        _ => {}
    }
}

pub fn show(state: &mut UiState, now: Instant) {
    state.visibility.visible = true;
    state.visibility.hide_deadline = None;
    state.visibility.idle_deadline = None;
    state.last_frame = now;
}

pub fn hide_immediately(state: &mut UiState, now: Instant) {
    state.visibility.visible = false;
    state.visibility.hide_deadline = Some(now);
    state.visibility.panel_opacity = 0.0;
}

pub fn set_transcript(state: &mut UiState, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    if state.transcript.text != trimmed {
        if trimmed.starts_with(state.transcript.text.as_str()) {
            state.transcript.bold_from = state.transcript.text.len();
        } else {
            state.transcript.bold_from = 0;
        }
        state.transcript.text = trimmed.to_string();
        state.transcript.text_pulse = 0.0;
        state.transcript.should_scroll_to_bottom = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_client::model::{InboundMessage, UiEventEnvelope};

    #[test]
    fn snapshot_with_recording_sets_visibility_and_preview() {
        let now = Instant::now();
        let mut state = UiState::new(now, Duration::from_secs(5), 40.0);

        apply_snapshot(
            &mut state,
            DaemonSnapshot {
                running: true,
                recording: true,
                transcript_preview: Some(" hello world ".to_string()),
            },
            now,
        );

        assert!(state.visibility.visible);
        assert_eq!(state.transcript.text, "hello world");
        assert!(state.transcript.should_scroll_to_bottom);
    }

    #[test]
    fn hide_event_hides_immediately() {
        let now = Instant::now();
        let mut state = UiState::new(now, Duration::from_secs(5), 40.0);
        let hide_linger = Duration::from_millis(250);
        state.visibility.visible = true;
        state.visibility.panel_opacity = 1.0;

        apply_message(
            &mut state,
            InboundMessage::UiEvent(UiEventEnvelope {
                v: 1,
                seq: 1,
                at_ms: 1,
                event_type: "ui.hide".to_string(),
                payload: serde_json::json!({"transcript": "done"}),
            }),
            now,
            hide_linger,
        );

        assert!(!state.visibility.visible);
        assert_eq!(state.transcript.text, "done");
        assert_eq!(state.visibility.hide_deadline, Some(now));
        assert_eq!(state.visibility.panel_opacity, 0.0);
    }

    #[test]
    fn transcript_delta_appends_and_bolds_new_segment() {
        let now = Instant::now();
        let mut state = UiState::new(now, Duration::from_secs(5), 40.0);
        state.transcript.text = "hello".to_string();

        apply_event(
            &mut state,
            UiEventEnvelope {
                v: 1,
                seq: 1,
                at_ms: 1,
                event_type: "transcript.delta".to_string(),
                payload: serde_json::json!({"delta": " world"}),
            },
            now,
            Duration::from_millis(250),
        );

        assert_eq!(state.transcript.text, "hello world");
        assert_eq!(state.transcript.bold_from, 5);
        assert!(state.visibility.visible);
    }
}
