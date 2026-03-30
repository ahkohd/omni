mod backend;
mod cli;
mod clipboard;
mod config;
mod daemon;
mod doctor;
mod hooks;
mod input;
mod recording;
mod ui_ipc;

use anyhow::{Result, bail};
use clap::Parser;

use crate::cli::{Cli, Command, ConfigCommand, InputCommand, TranscribeCommand};

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Start => daemon::start(cli.json)?,
        Command::Stop => daemon::stop(cli.json)?,
        Command::Status => daemon::status(cli.json)?,
        Command::Doctor => doctor::run(cli.json)?,
        Command::Reload => daemon::reload(cli.json)?,
        Command::Input { command } => match command {
            InputCommand::Show => input::show(cli.json)?,
            InputCommand::List => input::list(cli.json)?,
            InputCommand::Set { id, name } => input::set(id, name, cli.json)?,
        },
        Command::Transcribe { command } => match command {
            TranscribeCommand::Start {
                background,
                debug,
                debug_json,
            } => daemon::transcribe_start(background, debug, debug_json, cli.json)?,
            TranscribeCommand::Status => daemon::transcribe_status(cli.json)?,
            TranscribeCommand::Stop { mode } => daemon::transcribe_stop(mode, cli.json)?,
        },
        Command::Config { command } => handle_config_command(command, cli.json)?,
        Command::Daemon => daemon::run_foreground()?,
    }

    Ok(())
}

fn handle_config_command(command: ConfigCommand, json_output: bool) -> Result<()> {
    match command {
        ConfigCommand::Path => {
            let path = config::config_path()?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "path": path,
                    }))?
                );
            } else {
                println!("{}", path.display());
            }
        }
        ConfigCommand::Show => {
            let config = config::load_config()?;
            if json_output {
                println!("{}", serde_json::to_string_pretty(&config)?);
            } else {
                println!("{}", toml::to_string_pretty(&config)?);
            }
        }
        ConfigCommand::Get { key } => {
            let config = config::load_config()?;
            let Some(value) = config::get_value_by_key(&config, &key) else {
                bail!("config key not found: {key}");
            };

            if json_output {
                println!("{}", serde_json::to_string_pretty(value)?);
            } else {
                match value {
                    toml::Value::String(s) => println!("{s}"),
                    _ => println!("{value}"),
                }
            }
        }
        ConfigCommand::Set {
            key,
            value,
            json_value,
        } => {
            let mut config = config::load_config()?;
            let parsed = config::parse_set_value(&value, json_value)?;
            config::set_value_by_key(&mut config, &key, parsed)?;
            let path = config::save_config(&config)?;

            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "ok": true,
                        "action": "set",
                        "key": key,
                        "path": path,
                    }))?
                );
            } else {
                println!("updated {key} in {}", path.display());
            }
        }
        ConfigCommand::Unset { key } => {
            let mut config = config::load_config()?;
            let removed = config::unset_value_by_key(&mut config, &key)?;

            if !removed {
                bail!("config key not found: {key}");
            }

            let path = config::save_config(&config)?;

            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "ok": true,
                        "action": "unset",
                        "key": key,
                        "path": path,
                    }))?
                );
            } else {
                println!("removed {key} from {}", path.display());
            }
        }
    }

    Ok(())
}
