//! The command line: options first, then the command word, then the command's own arguments.
//!
//! Every token after the command word is positional, so a value such as `-2h` passes through as
//! written.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// What a command line asks for.
#[derive(Debug)]
pub(crate) enum Parsed {
    /// `--help`.
    Help,
    /// A command to run on a session.
    Run(Invocation),
}

/// The options and the command of one run.
#[derive(Debug)]
pub(crate) struct Invocation {
    pub(crate) picking: Picking,
    pub(crate) command: Command,
}

/// The options that say which session a command acts on.
#[derive(Debug, Default)]
pub(crate) struct Picking {
    /// `--session PATH`: the session file, outright.
    pub(crate) session: Option<PathBuf>,
    /// `--creator PID`: only the sessions that process created.
    pub(crate) creator: Option<u32>,
    /// `--newest`: the session created last.
    pub(crate) newest: bool,
    /// `--dir DIR`: the directory to look in, in place of the computed one.
    pub(crate) dir: Option<PathBuf>,
}

/// A command and its arguments, as typed. Values are checked when the command runs.
#[derive(Debug)]
pub(crate) enum Command {
    /// `list`.
    List,
    /// `info`.
    Info,
    /// `get <family>`.
    Get { family: String },
}

/// A command line that does not parse. The message names the offending token.
#[derive(Debug)]
pub(crate) struct Usage(pub(crate) String);

/// One line of `--help` per option and per command: the spelling, then what it does.
const OPTIONS: &[(&str, &str)] = &[
    (
        "--creator PID",
        "the session that process created; for the program you started",
    ),
    ("--newest", "the session created last"),
    ("--session PATH", "the path of the session file, outright"),
    (
        "--dir DIR",
        "look for sessions in DIR, not the usual directory",
    ),
];
const COMMANDS: &[(&str, &str)] = &[
    ("list", "every session in the directory"),
    (
        "info",
        "how the session was set up and what went wrong with it",
    ),
    ("get FAMILY", "what a family shows now"),
];

/// The one-line synopsis a usage error ends with.
pub(crate) const SYNOPSIS: &str = "usage: libtimewarp [options] <command> [arguments]";

/// The text `--help` prints.
pub(crate) fn help() -> String {
    let width = OPTIONS
        .iter()
        .chain(COMMANDS)
        .map(|(spelling, _)| spelling.len())
        .max()
        .unwrap_or(0);
    let rows = |table: &[(&str, &str)]| {
        table
            .iter()
            .map(|(spelling, what)| format!("  {spelling:width$}  {what}\n"))
            .collect::<Vec<_>>()
            .concat()
    };
    format!(
        "{SYNOPSIS}\n\nRead and change a live libtimewarp session.\n\ncommands:\n{}\n\
         picking the session:\n{}\n  {:width$}  show this help\n",
        rows(COMMANDS),
        rows(OPTIONS),
        "-h, --help",
    )
}

/// Parses the arguments after the program name.
pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Parsed, Usage> {
    let mut tokens = args.into_iter();
    let mut picking = Picking::default();
    let word = loop {
        let Some(token) = tokens.next() else {
            return Err(Usage("no command given".to_owned()));
        };
        if !token.as_bytes().starts_with(b"-") {
            break text(token)?;
        }
        let (name, attached) = split_option(&token);
        match name {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--session" => picking.session = Some(value(name, attached, &mut tokens)?.into()),
            "--dir" => picking.dir = Some(value(name, attached, &mut tokens)?.into()),
            "--creator" => picking.creator = Some(pid(name, &value(name, attached, &mut tokens)?)?),
            "--newest" if attached.is_none() => picking.newest = true,
            "--newest" => return Err(Usage("--newest takes no value".to_owned())),
            _ => return Err(Usage(format!("unknown option {}", token.to_string_lossy()))),
        }
    };
    let rest = tokens.map(text).collect::<Result<Vec<_>, _>>()?;
    Ok(Parsed::Run(Invocation {
        picking,
        command: command(&word, rest)?,
    }))
}

/// The command `word` names, with its arguments.
fn command(word: &str, rest: Vec<String>) -> Result<Command, Usage> {
    match word {
        "list" => {
            exactly::<0>(word, rest)?;
            Ok(Command::List)
        }
        "info" => {
            exactly::<0>(word, rest)?;
            Ok(Command::Info)
        }
        "get" => {
            let [family] = exactly::<1>(word, rest)?;
            Ok(Command::Get { family })
        }
        _ => Err(Usage(format!("unknown command {word}"))),
    }
}

/// `rest` as exactly `N` arguments of the command `word`.
fn exactly<const N: usize>(word: &str, rest: Vec<String>) -> Result<[String; N], Usage> {
    rest.try_into().map_err(|rest: Vec<String>| {
        Usage(match rest.get(N..) {
            Some(extra) if !extra.is_empty() => {
                format!("unexpected after {word}: {}", extra.join(" "))
            }
            _ => format!("{word} is missing an argument"),
        })
    })
}

/// An option token as its name and the value attached with `=`, if any. A name that is not UTF-8
/// comes back empty, which matches no option.
fn split_option(token: &OsStr) -> (&str, Option<&OsStr>) {
    let bytes = token.as_bytes();
    let (name, attached) =
        bytes
            .iter()
            .position(|&byte| byte == b'=')
            .map_or((bytes, None), |at| {
                (
                    bytes.get(..at).unwrap_or_default(),
                    bytes.get(at + 1..).map(OsStr::from_bytes),
                )
            });
    (std::str::from_utf8(name).unwrap_or_default(), attached)
}

/// The value of the option `name`: the text after `=`, else the next token.
fn value(
    name: &str,
    attached: Option<&OsStr>,
    tokens: &mut impl Iterator<Item = OsString>,
) -> Result<OsString, Usage> {
    attached
        .map(OsStr::to_os_string)
        .or_else(|| tokens.next())
        .ok_or_else(|| Usage(format!("{name} takes a value")))
}

/// The value of the option `name` as a process id.
fn pid(name: &str, value: &OsStr) -> Result<u32, Usage> {
    value
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| {
            Usage(format!(
                "{name} takes a process id, not {}",
                value.to_string_lossy()
            ))
        })
}

/// A token that has to be text: the command word and its arguments.
fn text(token: OsString) -> Result<String, Usage> {
    token
        .into_string()
        .map_err(|token| Usage(format!("{} is not UTF-8", token.to_string_lossy())))
}
