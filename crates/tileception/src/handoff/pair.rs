//! `tileception pair`: the handoff passphrase and switch from a terminal, so
//! pairing another computer needs no config editing. The running session
//! picks changes up when config.toml is saved.

use std::{
    io::{BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
};

use crate::config_edit::{get_value, set_value};

pub const HELP: &str = "\
Usage: tileception pair [--file <path>] [<passphrase words...> | --new | --off | --show]

Handoff shares this mouse, keyboard and clipboard with other Tileception
computers that know the same passphrase.

  (nothing)        show the passphrase and connected computers; in a terminal,
                   offer to turn handoff on, choosing three random words when
                   there is no passphrase yet
  <words...>       join: use the passphrase another computer shows, and turn
                   handoff on
  --new            choose three new random words (paired computers then need
                   them too) and turn handoff on
  --off            turn handoff off, keeping the passphrase
  --show           only show, change nothing

Options:
  --file <path>   use this config instead of $XDG_CONFIG_HOME/tileception/config.toml

Examples:
  tileception pair
  tileception pair citizen calzone dazzler
";

pub fn run(args: &[String]) -> Result<(), String> {
    let (file, args) = match args {
        [flag, path, rest @ ..] if flag == "--file" => (PathBuf::from(path), rest),
        _ => (crate::config::path(), args),
    };
    match args {
        [help] if matches!(help.as_str(), "-h" | "--help" | "help") => {
            print!("{HELP}");
            return Ok(());
        }
        [] => {
            let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
            if interactive && !enabled(&file) && ask("Handoff is off. Turn it on?")? {
                turn_on(&file, passphrase(&file).map_or_else(super::suggest_passphrase, Ok)?)?;
            }
        }
        [flag] if flag == "--show" => {}
        [flag] if flag == "--off" => {
            set_value(&file, "handoff.enabled", "false")?;
        }
        [flag] if flag == "--new" => turn_on(&file, super::suggest_passphrase()?)?,
        [flag, ..] if flag.starts_with('-') => {
            return Err(format!("unknown option {flag}; see tileception pair --help"));
        }
        words => {
            let phrase = words.join(" ");
            if phrase.trim().chars().count() < 12 {
                eprintln!("That passphrase is short: several random words are much harder to guess.");
            }
            turn_on(&file, phrase.trim().to_string())?;
        }
    }
    show(&file);
    Ok(())
}

fn enabled(file: &Path) -> bool {
    get_value(file, "handoff.enabled").is_ok_and(|value| value == "true")
}

fn passphrase(file: &Path) -> Option<String> {
    get_value(file, "handoff.passphrase").ok().filter(|phrase| !phrase.trim().is_empty())
}

/// The passphrase first: enabled without one would only log a warning.
fn turn_on(file: &Path, phrase: String) -> Result<(), String> {
    set_value(file, "handoff.passphrase", &phrase)?;
    set_value(file, "handoff.enabled", "true")
}

fn ask(question: &str) -> Result<bool, String> {
    print!("{question} [Y/n] ");
    std::io::stdout().flush().map_err(|error| error.to_string())?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).map_err(|error| error.to_string())?;
    Ok(matches!(answer.trim(), "" | "y" | "Y" | "yes" | "Yes"))
}

fn show(file: &Path) {
    let active = enabled(file);
    let name = get_value(file, "handoff.name").unwrap_or_else(|_| super::host_name());
    let port = get_value(file, "handoff.port").unwrap_or_else(|_| tileception_handoff::DEFAULT_PORT.to_string());
    match (active, passphrase(file)) {
        (true, Some(phrase)) => {
            println!("Handoff is on; others see this computer as {name} (TCP port {port}).");
            println!("Passphrase: {phrase}");
            println!();
            println!("On each other computer, run:");
            println!("  tileception pair {phrase}");
        }
        (true, None) => println!("Handoff is on but has no passphrase, so nothing connects: tileception pair --new"),
        (false, Some(_)) => println!("Handoff is off. tileception pair turns it on with the same passphrase."),
        (false, None) => println!("Handoff is off. tileception pair turns it on."),
    }
    if !active {
        return;
    }
    // Only when a session runs; the config is enough otherwise.
    let Ok(reply) = crate::ipc::request(&["handoff".to_string()]) else { return };
    if reply.starts_with("error") || reply.starts_with("handoff is off") {
        return;
    }
    let machines: Vec<&str> = reply.lines().filter(|line| !line.starts_with("listening on")).collect();
    println!();
    if machines.is_empty() {
        println!("No other computer is connected yet. Once one is, drag its card to the");
        println!("screen edge where it stands; Ctrl+Super+Escape brings input back.");
    } else {
        println!("Computers:");
        for machine in machines {
            println!("  {machine}");
        }
    }
}
