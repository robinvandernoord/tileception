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
                   there is no passphrase yet, to show a dismissed pairing
                   card again, and to bring back windows left on a computer
                   that went away
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
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let mut offer = interactive;
    match args {
        [help] if matches!(help.as_str(), "-h" | "--help" | "help") => {
            print!("{HELP}");
            return Ok(());
        }
        [] => {
            if interactive && !enabled(&file) && ask("Handoff is off. Turn it on?")? {
                turn_on(&file, passphrase(&file).map_or_else(super::suggest_passphrase, Ok)?)?;
            }
        }
        [flag] if flag == "--show" => offer = false,
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
    show(&file, offer)
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

/// Shows the state of handoff; with `offer`, asks about what can be done.
fn show(file: &Path, offer: bool) -> Result<(), String> {
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
        return Ok(());
    }
    // Only when a session runs; the config is enough otherwise.
    let Ok(reply) = crate::ipc::request(&["handoff".to_string()]) else { return Ok(()) };
    if reply.starts_with("error") || reply.starts_with("handoff is off") {
        return Ok(());
    }
    let machines: Vec<&str> = reply.lines().filter(|line| !line.starts_with("listening on")).collect();
    println!();
    if machines.is_empty() {
        println!("No other computer is connected yet. Once one is, drag its card to the");
        println!("screen edge where it stands; Ctrl+Super+Escape brings input back.");
    } else {
        println!("Computers:");
        for machine in &machines {
            println!("  {machine}");
        }
    }
    let dismissed = dismissed(&machines);
    if !dismissed.is_empty() {
        let names = dismissed.join(", ");
        println!();
        if offer && ask(&format!("The pairing card for {names} was dismissed. Show it again?"))? {
            command(&["handoff", "retry"])?;
            println!("Drag the card to the screen edge where {names} stands.");
        } else {
            println!("To show the pairing card for {names} again: tileception msg handoff retry");
        }
    }
    for (machine, count) in waiting(&machines) {
        let windows = if count == 1 { "1 window".to_string() } else { format!("{count} windows") };
        println!();
        if offer && ask(&format!("{windows} wait for {machine}, which went away. Bring them back here?"))? {
            command(&["handoff", "reclaim", machine])?;
        } else {
            println!("{windows} wait for {machine}; to bring them back: tileception msg handoff reclaim {machine}");
        }
    }
    Ok(())
}

/// Runs a command in the session.
fn command(args: &[&str]) -> Result<(), String> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let reply = crate::ipc::request(&args).map_err(|error| format!("cannot reach the session: {error}"))?;
    match reply.strip_prefix("error") {
        Some(error) => Err(error.trim_start_matches(':').trim().to_string()),
        None => Ok(()),
    }
}

/// The machines whose pairing card was dismissed, from `handoff` status.
fn dismissed<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    lines
        .iter()
        .filter_map(|line| line.strip_suffix(" dismissed"))
        .filter_map(|rest| rest.rsplit_once(' ').map(|(name, _)| name))
        .collect()
}

/// This machine's windows waiting on machines that went away, counted per
/// machine, from `handoff` status.
fn waiting<'a>(lines: &[&'a str]) -> std::collections::BTreeMap<&'a str, usize> {
    let mut counts = std::collections::BTreeMap::new();
    for line in lines {
        if let Some((_, machine)) = line
            .strip_prefix("window ")
            .and_then(|line| line.strip_suffix(" (waiting for it to link again)"))
            .and_then(|line| line.rsplit_once(" is on "))
        {
            *counts.entry(machine).or_insert(0) += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_tells_what_can_be_done() {
        let status = [
            "laptop 00000000000000a1 dismissed",
            "Robin's desk 00000000000000b2 linked at eDP-1 left",
            "window 4 \"notes\" is on laptop (waiting for it to link again)",
            "window 9 \"is on\" is on laptop (waiting for it to link again)",
            "window 7 \"build\" is on Robin's desk",
        ];
        assert_eq!(dismissed(&status), vec!["laptop"]);
        assert_eq!(waiting(&status).into_iter().collect::<Vec<_>>(), vec![("laptop", 2)]);
    }
}
