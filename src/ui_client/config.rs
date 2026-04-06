use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

pub const DEFAULT_IDLE_MS: u64 = 30_000;
pub const DEFAULT_HIDE_MS: u64 = 250;
pub const DEFAULT_RETRY_MS: u64 = 220;

pub const PILL_WIDTH: f32 = 220.0;
pub const WINDOW_WIDTH: f32 = PILL_WIDTH;
pub const SAFE_BOTTOM_MARGIN: f32 = 20.0;

pub const TRANSCRIPT_FADE_EDGE: f32 = 18.0;
pub const TRANSCRIPT_MAX_LINES: usize = 4;
pub const TRANSCRIPT_CHARS_PER_LINE_ESTIMATE: usize = 38;

pub const PILL_WAVE_HEIGHT: f32 = 40.0;
pub const PILL_TRANSCRIPT_MIN_HEIGHT: f32 = 60.0;
pub const PILL_TRANSCRIPT_MAX_HEIGHT: f32 = 120.0;
pub const PILL_TRANSCRIPT_BASE_HEIGHT: f32 = 40.0;
pub const TRANSCRIPT_LINE_HEIGHT: f32 = 20.0;
pub const WINDOW_HEIGHT: f32 = PILL_TRANSCRIPT_MAX_HEIGHT;

#[derive(Debug, Parser)]
#[command(
    name = "omni-transcribe-ui",
    version,
    about = "Bottom-center transcription pill UI client for omni ui.sock"
)]
pub struct Cli {
    /// Override ui event socket path.
    #[arg(long)]
    pub socket: Option<PathBuf>,

    /// Override daemon command socket path.
    #[arg(long)]
    pub daemon_socket: Option<PathBuf>,

    /// Keep process alive for this long after hide, then quit if unused.
    #[arg(long, default_value_t = DEFAULT_IDLE_MS)]
    pub idle_ms: u64,

    /// Delay hide transitions by this many milliseconds.
    #[arg(long, default_value_t = DEFAULT_HIDE_MS)]
    pub hide_ms: u64,

    /// Socket reconnect retry in milliseconds.
    #[arg(long, default_value_t = DEFAULT_RETRY_MS)]
    pub retry_ms: u64,
}

#[derive(Debug, Clone)]
pub struct UiRuntimeConfig {
    pub ui_socket: PathBuf,
    pub daemon_socket: PathBuf,
    pub idle_linger: Duration,
    pub hide_linger: Duration,
    pub reconnect_delay: Duration,
}
