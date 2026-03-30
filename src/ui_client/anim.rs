use std::time::{Duration, Instant};

use crate::ui_client::model::UiState;

#[derive(Debug, Clone, Copy)]
pub struct AnimationConfig {
    pub idle_linger: Duration,
    pub transcript_chars_per_line_estimate: usize,
    pub transcript_max_lines: usize,
    pub pill_wave_height: f32,
    pub pill_transcript_min_height: f32,
    pub pill_transcript_max_height: f32,
    pub pill_transcript_base_height: f32,
    pub transcript_line_height: f32,
    pub aurora_idle_flow_speed: f32,
    pub aurora_flow_speed_boost: f32,
    pub aurora_rms_gain: f32,
    pub aurora_peak_gain: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickAction {
    None,
    RemoveWindow,
}

pub fn normalized_audio_energy(state: &UiState, cfg: AnimationConfig) -> f32 {
    (state.audio.energy_display * cfg.aurora_rms_gain
        + state.audio.peak_display * cfg.aurora_peak_gain)
        .clamp(0.0, 1.0)
}

pub fn estimated_transcript_lines(state: &UiState, chars_per_line_estimate: usize) -> usize {
    let text = state.transcript.text.trim();
    if text.is_empty() {
        return 1;
    }

    text.lines()
        .map(|line| {
            let chars = line.chars().count();
            let wrapped = chars.div_ceil(chars_per_line_estimate);
            wrapped.max(1)
        })
        .sum::<usize>()
        .max(1)
}

pub fn target_shell_height(state: &UiState, cfg: AnimationConfig) -> f32 {
    if state.transcript.text.trim().is_empty() {
        return cfg.pill_wave_height;
    }

    let visible_lines = estimated_transcript_lines(state, cfg.transcript_chars_per_line_estimate)
        .clamp(1, cfg.transcript_max_lines);

    (cfg.pill_transcript_base_height + visible_lines as f32 * cfg.transcript_line_height).clamp(
        cfg.pill_transcript_min_height,
        cfg.pill_transcript_max_height,
    )
}

pub fn tick(state: &mut UiState, now: Instant, cfg: AnimationConfig) -> TickAction {
    let dt = (now - state.last_frame).as_secs_f32().clamp(0.0, 0.050);
    state.last_frame = now;

    if let Some(deadline) = state.visibility.hide_deadline
        && now >= deadline
    {
        state.visibility.hide_deadline = None;
        state.visibility.visible = false;
        state.visibility.idle_deadline = Some(now + cfg.idle_linger);
    }

    if !state.visibility.visible
        && state.visibility.hide_deadline.is_none()
        && let Some(deadline) = state.visibility.idle_deadline
        && now >= deadline
    {
        return TickAction::RemoveWindow;
    }

    let wave_speed = if state.audio.silent {
        1.2
    } else {
        2.8 + state.audio.energy_display * 1.5
    };
    state.audio.wave_phase = (state.audio.wave_phase + dt * wave_speed) % std::f32::consts::TAU;

    let energy_rate = if state.audio.rms > state.audio.energy_display {
        18.0
    } else {
        2.8 + state.audio.energy_display * 1.5
    };
    state.audio.energy_display +=
        (state.audio.rms - state.audio.energy_display) * (1.0 - (-dt * energy_rate).exp());

    let peak_rate = if state.audio.peak > state.audio.peak_display {
        40.0
    } else {
        8.0
    };
    state.audio.peak_display +=
        (state.audio.peak - state.audio.peak_display) * (1.0 - (-dt * peak_rate).exp());

    state.transcript.text_pulse = (state.transcript.text_pulse + dt * 2.8).min(1.0);

    let aurora_energy = normalized_audio_energy(state, cfg);
    let aurora_speed = cfg.aurora_idle_flow_speed + aurora_energy * cfg.aurora_flow_speed_boost;
    state.aurora.time += dt * aurora_speed;
    if state.aurora.time >= 10_000.0 {
        state.aurora.time -= 10_000.0;
    }

    let target_fill = if state.transcript.text.trim().is_empty() {
        1.0
    } else {
        let lines = estimated_transcript_lines(state, cfg.transcript_chars_per_line_estimate)
            .clamp(1, cfg.transcript_max_lines) as f32;
        1.0 - (lines / cfg.transcript_max_lines as f32)
    };
    state.transcript.fill_state +=
        (target_fill - state.transcript.fill_state) * (1.0 - (-dt * 8.0).exp());

    let shell_target = target_shell_height(state, cfg);
    state.transcript.shell_height +=
        (shell_target - state.transcript.shell_height) * (1.0 - (-dt * 12.0).exp());

    if (shell_target - state.transcript.shell_height).abs() <= 0.2 {
        state.transcript.shell_height = shell_target;
    }

    let target_opacity = if state.visibility.visible || state.visibility.hide_deadline.is_some() {
        1.0
    } else {
        0.0
    };
    state.visibility.panel_opacity +=
        (target_opacity - state.visibility.panel_opacity) * (1.0 - (-dt * 11.0).exp());

    if (target_opacity - state.visibility.panel_opacity).abs() <= 0.01 {
        state.visibility.panel_opacity = target_opacity;
    }

    TickAction::None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AnimationConfig {
        AnimationConfig {
            idle_linger: Duration::from_millis(500),
            transcript_chars_per_line_estimate: 10,
            transcript_max_lines: 4,
            pill_wave_height: 40.0,
            pill_transcript_min_height: 60.0,
            pill_transcript_max_height: 120.0,
            pill_transcript_base_height: 40.0,
            transcript_line_height: 20.0,
            aurora_idle_flow_speed: 0.25,
            aurora_flow_speed_boost: 0.5,
            aurora_rms_gain: 24.0,
            aurora_peak_gain: 7.0,
        }
    }

    #[test]
    fn removes_window_after_idle_deadline() {
        let now = Instant::now();
        let mut state = UiState::new(now, Duration::from_millis(500), 40.0);
        state.visibility.visible = false;
        state.visibility.hide_deadline = None;
        state.visibility.idle_deadline = Some(now + Duration::from_millis(100));
        state.last_frame = now;

        let action = tick(&mut state, now + Duration::from_millis(120), cfg());
        assert_eq!(action, TickAction::RemoveWindow);
    }

    #[test]
    fn target_shell_height_grows_with_more_lines() {
        let now = Instant::now();
        let mut state = UiState::new(now, Duration::from_millis(500), 40.0);
        state.transcript.text = "short".to_string();
        let single = target_shell_height(&state, cfg());

        state.transcript.text = "line one\nline two\nline three".to_string();
        let multi = target_shell_height(&state, cfg());

        assert!(multi >= single);
        assert!(multi <= cfg().pill_transcript_max_height);
    }

    #[test]
    fn normalized_energy_is_clamped() {
        let now = Instant::now();
        let mut state = UiState::new(now, Duration::from_millis(500), 40.0);
        state.audio.energy_display = 999.0;
        state.audio.peak_display = 999.0;

        let energy = normalized_audio_energy(&state, cfg());
        assert_eq!(energy, 1.0);
    }
}
