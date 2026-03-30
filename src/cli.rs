use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "omni",
    version,
    about = "Tiny keybind-first realtime transcription CLI"
)]
pub struct Cli {
    /// Emit machine-readable output where supported.
    #[arg(long, global = true)]
    pub json: bool,

    /// Emit simple profiling timing where supported.
    #[arg(long, global = true)]
    pub profile: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the omni daemon.
    Start,

    /// Stop the omni daemon.
    Stop,

    /// Show daemon status.
    Status,

    /// Run health and environment checks.
    Doctor,

    /// Reload runtime configuration.
    Reload,

    /// Manage input devices.
    Input {
        #[command(subcommand)]
        command: InputCommand,
    },

    /// Manage transcription lifecycle.
    #[command(name = "transcribe")]
    Transcribe {
        #[command(subcommand)]
        command: TranscribeCommand,
    },

    /// Manage configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Internal daemon entrypoint (not for users).
    #[command(name = "__daemon", hide = true)]
    Daemon,
}

#[derive(Debug, Subcommand)]
pub enum InputCommand {
    /// Show current configured input device.
    Show,

    /// List available input devices and IDs.
    List,

    /// Set input device by ID from `input list`, or by exact name with --name.
    Set {
        /// Device ID from `input list`, or `default`.
        #[arg(required_unless_present = "name")]
        id: Option<String>,

        /// Exact input device name.
        #[arg(long, conflicts_with = "id")]
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum TranscribeCommand {
    /// Begin realtime transcription capture.
    Start {
        /// Run in background mode without attaching live terminal preview.
        #[arg(long = "background", visible_alias = "bg")]
        background: bool,

        /// Print live debug diagnostics (event/commit counters) while attached.
        #[arg(long)]
        debug: bool,

        /// Emit compact JSON lines for each newly seen realtime event.
        #[arg(long = "debug-json")]
        debug_json: bool,
    },

    /// Show active transcription state.
    Status,

    /// Stop transcription capture; mode is optional and maps to transcribe.stop_<mode> hooks.
    Stop {
        /// Optional mode, e.g. copy, insert, slack.
        mode: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print active config file path.
    Path,

    /// Show merged config values.
    Show,

    /// Get one config key by dot path.
    Get { key: String },

    /// Set one config key by dot path.
    Set {
        key: String,
        value: String,

        /// Parse value as JSON instead of raw string.
        #[arg(long)]
        json_value: bool,
    },

    /// Remove one config key by dot path.
    Unset { key: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_input_show() {
        let cli = Cli::parse_from(["omni", "input", "show"]);
        match cli.command {
            Command::Input {
                command: InputCommand::Show,
            } => {}
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_input_list() {
        let cli = Cli::parse_from(["omni", "input", "list"]);
        match cli.command {
            Command::Input {
                command: InputCommand::List,
            } => {}
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_input_set() {
        let cli = Cli::parse_from(["omni", "input", "set", "3"]);
        match cli.command {
            Command::Input {
                command: InputCommand::Set { id, name },
            } => {
                assert_eq!(id.as_deref(), Some("3"));
                assert!(name.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_input_set_by_name() {
        let cli = Cli::parse_from(["omni", "input", "set", "--name", "Logitech BRIO"]);
        match cli.command {
            Command::Input {
                command: InputCommand::Set { id, name },
            } => {
                assert!(id.is_none());
                assert_eq!(name.as_deref(), Some("Logitech BRIO"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_transcribe_start_default_live() {
        let cli = Cli::parse_from(["omni", "transcribe", "start"]);
        match cli.command {
            Command::Transcribe {
                command:
                    TranscribeCommand::Start {
                        background,
                        debug,
                        debug_json,
                    },
            } => {
                assert!(!background);
                assert!(!debug);
                assert!(!debug_json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_transcribe_start_background_aliases() {
        let cli = Cli::parse_from(["omni", "transcribe", "start", "--background"]);
        match cli.command {
            Command::Transcribe {
                command: TranscribeCommand::Start { background, .. },
            } => assert!(background),
            other => panic!("unexpected command: {other:?}"),
        }

        let cli = Cli::parse_from(["omni", "transcribe", "start", "--bg"]);
        match cli.command {
            Command::Transcribe {
                command: TranscribeCommand::Start { background, .. },
            } => assert!(background),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_transcribe_start_debug_flags() {
        let cli = Cli::parse_from(["omni", "transcribe", "start", "--debug", "--debug-json"]);
        match cli.command {
            Command::Transcribe {
                command:
                    TranscribeCommand::Start {
                        debug, debug_json, ..
                    },
            } => {
                assert!(debug);
                assert!(debug_json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_transcribe_stop_without_mode() {
        let cli = Cli::parse_from(["omni", "transcribe", "stop"]);
        match cli.command {
            Command::Transcribe {
                command: TranscribeCommand::Stop { mode },
            } => assert!(mode.is_none()),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_transcribe_stop_with_mode() {
        let cli = Cli::parse_from(["omni", "transcribe", "stop", "insert"]);
        match cli.command {
            Command::Transcribe {
                command: TranscribeCommand::Stop { mode },
            } => assert_eq!(mode.as_deref(), Some("insert")),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_transcribe_status() {
        let cli = Cli::parse_from(["omni", "transcribe", "status"]);
        match cli.command {
            Command::Transcribe {
                command: TranscribeCommand::Status,
            } => {}
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_config_set_with_json_value() {
        let cli = Cli::parse_from([
            "omni",
            "config",
            "set",
            "event.hooks.transcribe.stop",
            "[\"hide_ui\",\"copy\"]",
            "--json-value",
        ]);

        match cli.command {
            Command::Config {
                command:
                    ConfigCommand::Set {
                        key,
                        value,
                        json_value,
                    },
            } => {
                assert_eq!(key, "event.hooks.transcribe.stop");
                assert_eq!(value, "[\"hide_ui\",\"copy\"]");
                assert!(json_value);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
