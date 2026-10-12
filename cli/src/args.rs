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
    /// `--pid PID`: the session that process carries the path of in its environment.
    pub(crate) pid: Option<u32>,
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
    /// `shift`, `set-time`, or `set-rate`, then any further operations, staged in order on one
    /// transaction.
    Change(Vec<Operation>),
}

/// One operation of a change, as typed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Operation {
    pub(crate) kind: Kind,
    pub(crate) family: String,
    pub(crate) value: String,
}

/// The operations a change is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `shift <family> <duration>`.
    Shift,
    /// `set-time <family> <value>`.
    SetTime,
    /// `set-rate <family> <rate>`.
    SetRate,
}

impl Kind {
    /// The operation `word` names.
    fn named(word: &str) -> Option<Self> {
        [Self::Shift, Self::SetTime, Self::SetRate]
            .into_iter()
            .find(|kind| kind.word() == word)
    }

    /// The word that names the operation.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Shift => "shift",
            Self::SetTime => "set-time",
            Self::SetRate => "set-rate",
        }
    }
}

/// A command line that does not parse. The message names the offending token.
#[derive(Debug)]
pub(crate) struct Usage(pub(crate) String);

/// One line of `--help` per option and per command: the spelling, then what it does.
const OPTIONS: &[(&str, &str)] = &[
    (
        "--pid PID",
        "the session that process of the faked tree carries in its environment",
    ),
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
    (
        "shift FAMILY DURATION",
        "move a family by a duration such as -2h, 90s or 500ms",
    ),
    (
        "set-time FAMILY VALUE",
        "make a family show 2020-01-01T00:00:00Z, @1577836800, or a duration",
    ),
    ("set-rate FAMILY RATE", "fake seconds per second; 0 freezes"),
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
         shift, set-time, and set-rate take further operations after their own, each written\n\
         as a command; all of them are applied together or not at all:\n\
         \x20 libtimewarp shift realtime -2h set-rate realtime 10\n\n\
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
            "--pid" => picking.pid = Some(pid(name, &value(name, attached, &mut tokens)?)?),
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
        _ => {
            let kind = Kind::named(word).ok_or_else(|| Usage(format!("unknown command {word}")))?;
            operations(kind, rest).map(Command::Change)
        }
    }
}

/// The operation `first` with its two arguments, then each further operation in `rest` with its
/// two.
fn operations(first: Kind, rest: Vec<String>) -> Result<Vec<Operation>, Usage> {
    let mut tokens = rest.into_iter();
    let mut operations = Vec::new();
    let mut kind = first;
    loop {
        let (Some(family), Some(value)) = (tokens.next(), tokens.next()) else {
            return Err(Usage(format!("{} is missing an argument", kind.word())));
        };
        operations.push(Operation {
            kind,
            family,
            value,
        });
        let Some(word) = tokens.next() else {
            return Ok(operations);
        };
        kind = Kind::named(&word).ok_or_else(|| {
            Usage(format!(
                "unexpected {word}: a further operation is shift, set-time, or set-rate"
            ))
        })?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(line: &[&str]) -> Result<Command, Usage> {
        match parse(line.iter().map(OsString::from))? {
            Parsed::Run(invocation) => Ok(invocation.command),
            Parsed::Help => Err(Usage("help".to_owned())),
        }
    }

    fn operation(kind: Kind, family: &str, value: &str) -> Operation {
        Operation {
            kind,
            family: family.to_owned(),
            value: value.to_owned(),
        }
    }

    fn usage(line: &[&str]) -> String {
        match parsed(line) {
            Err(Usage(message)) => message,
            Ok(command) => format!("parsed as {command:?}"),
        }
    }

    #[test]
    fn a_sequence_is_every_triple_in_order() {
        let Ok(Command::Change(operations)) = parsed(&[
            "shift",
            "realtime",
            "-2h",
            "set-rate",
            "realtime",
            "10",
            "set-time",
            "monotonic",
            "1d",
        ]) else {
            panic!("not a change");
        };
        assert_eq!(
            operations,
            [
                operation(Kind::Shift, "realtime", "-2h"),
                operation(Kind::SetRate, "realtime", "10"),
                operation(Kind::SetTime, "monotonic", "1d"),
            ]
        );
    }

    #[test]
    fn a_malformed_triple_is_named() {
        assert_eq!(
            usage(&["set-time", "realtime"]),
            "set-time is missing an argument"
        );
        assert_eq!(
            usage(&["shift", "realtime", "1h", "set-rate", "monotonic"]),
            "set-rate is missing an argument"
        );
        assert!(usage(&["shift", "realtime", "1h", "get", "monotonic", "1"]).contains("get"));
        assert!(usage(&["shift", "realtime", "1h", "extra"]).contains("extra"));
    }
}
