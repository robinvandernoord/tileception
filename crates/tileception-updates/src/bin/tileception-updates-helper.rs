//! Runs one of the update manager's tasks as root. Polkit starts it through
//! `pkexec`, under the policy `org.tileception.updates`: `refresh` for the
//! person at the computer without a password, `change ...` after an
//! administrator authenticated. It takes a task, never a command line: the
//! programs and their arguments are fixed in `tileception_updates::task`,
//! and package names are checked before anything runs.
//!
//! What the programs print goes to standard output as it comes. A failure
//! ends with its reason on standard error and exit status 1; a request that
//! is not a task ends with status 2.

use std::process::ExitCode;

use tileception_updates::task::{self, FAILED, Task};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let task = match Task::from_args(&args) {
        Ok(task) => task,
        Err(error) => {
            eprintln!("{FAILED}{error}");
            return ExitCode::from(2);
        }
    };
    if !task::is_root() {
        eprintln!("{FAILED}this helper changes packages and must run as root; Tileception starts it through pkexec");
        return ExitCode::from(2);
    }
    match task::run(&task, &mut |line| println!("{line}")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("{FAILED}{reason}");
            ExitCode::FAILURE
        }
    }
}
