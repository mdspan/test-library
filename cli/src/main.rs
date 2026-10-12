//! `libtimewarp`: reads and changes a live session from the shell.
//!
//! Options come before the command word; output is `key=value` lines on stdout. A refusal goes to
//! stderr and exits 1, a usage error exits 2.

mod args;
mod change;
mod commands;
mod directory;
mod environ;
mod failure;
mod family;
mod format;
mod frozen;
mod output;

use std::io::{self, BufWriter};
use std::process::ExitCode;

use args::Parsed;
use failure::Failure;
use output::Output;

fn main() -> ExitCode {
    let mut out = Output::new(BufWriter::new(io::stdout().lock()));
    let ran = match args::parse(std::env::args_os().skip(1)) {
        Ok(Parsed::Help) => out.text(&args::help()),
        Ok(Parsed::Run(invocation)) => commands::run(&invocation, &mut out),
        Err(usage) => Err(Failure::from(usage)),
    };
    let flushed = out.finish();
    match ran.and(flushed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => failure.report(),
    }
}
