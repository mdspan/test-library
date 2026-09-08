//! The destination and the line, exercised without touching the process-wide logger: a test builds
//! its own [`Sink`] and reads back what came out the other end.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Read as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::fs::PermissionsExt as _;

use super::*;

/// An environment holding exactly the pairs given.
fn env<'a>(pairs: &'a [(&'static str, String)]) -> impl EnvLookup + use<'a> {
    move |name: &str| -> Option<OsString> {
        pairs
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, value)| value.clone().into())
    }
}

/// A pipe, as the read and write ends of one.
fn pipe() -> (std::fs::File, std::fs::File) {
    let mut ends = [-1, -1];
    // Safety: `ends` is a live buffer of the two `int`s `pipe` fills.
    assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
    // Safety: the number is a fresh descriptor this test now owns.
    let file = |fd| unsafe { std::fs::File::from_raw_fd(fd) };
    (file(ends[0]), file(ends[1]))
}

/// Everything written to the pipe so far. One read, because our lines are one `write` each and
/// reading to the end would block on the write end this test still holds.
fn drain(read_end: &mut std::fs::File) -> String {
    let mut buf = [0u8; 4096];
    let got = read_end.read(&mut buf).unwrap();
    String::from_utf8_lossy(&buf[..got]).into_owned()
}

#[test]
fn a_level_is_named_by_the_word_a_user_writes() {
    for level in Level::ALL {
        assert_eq!(Level::parse(level.as_str()), Some(level));
    }
    assert_eq!(Level::parse(" ERROR "), Some(Level::Error));
    assert_eq!(Level::parse("chatty"), None);
    assert_eq!(Level::parse(""), None);
}

#[test]
fn a_destination_keeps_its_level_and_everything_above_it() {
    let sink = Sink {
        descriptor: -1,
        level: Level::Warn,
    };
    assert!(sink.enabled(Level::Error));
    assert!(sink.enabled(Level::Warn));
    assert!(!sink.enabled(Level::Info));
    assert!(!sink.enabled(Level::Trace));
}

#[test]
fn an_unset_or_unreadable_level_falls_back_to_info() {
    let (_read_end, write_end) = pipe();
    let fd = write_end.as_raw_fd().to_string();
    for level in ["", "chatty"] {
        let sink = Sink::from_env(&env(&[
            (ENV_LOG_FD, fd.clone()),
            (ENV_LOG_LEVEL, level.to_owned()),
        ]))
        .unwrap();
        assert!(sink.enabled(Level::Info));
        assert!(!sink.enabled(Level::Debug));
    }
}

#[test]
fn naming_no_destination_installs_nothing() {
    assert!(Sink::from_env(&env(&[(ENV_LOG_LEVEL, "trace".to_owned())])).is_none());
    assert!(Sink::from_env(&env(&[])).is_none());
}

#[test]
fn a_line_names_the_process_and_the_level() {
    let (mut read_end, write_end) = pipe();
    let sink = Sink::from_env(&env(&[(ENV_LOG_FD, write_end.as_raw_fd().to_string())])).unwrap();

    sink.write(
        Level::Error,
        format_args!("{}: not a valid value", "SOME_VAR"),
    );

    // Safety: `getpid` reads the caller's own process and cannot fail.
    let pid = unsafe { libc::getpid() };
    assert_eq!(
        drain(&mut read_end),
        format!("libtimewarp[{pid}] error: SOME_VAR: not a valid value\n")
    );
}

#[test]
fn a_line_below_the_level_is_not_written() {
    let (mut read_end, write_end) = pipe();
    let sink = Sink::from_env(&env(&[
        (ENV_LOG_FD, write_end.as_raw_fd().to_string()),
        (ENV_LOG_LEVEL, "error".to_owned()),
    ]))
    .unwrap();

    sink.write(Level::Warn, format_args!("dropped"));
    sink.write(Level::Error, format_args!("kept"));

    assert!(drain(&mut read_end).ends_with("error: kept\n"));
}

#[test]
fn a_named_descriptor_is_written_through_a_private_copy() {
    let (mut read_end, write_end) = pipe();
    let named = write_end.as_raw_fd();
    let sink = Sink::from_env(&env(&[(ENV_LOG_FD, named.to_string())])).unwrap();

    assert_ne!(sink.descriptor(), named);
    assert!(sink.descriptor() >= private_fd_floor());

    // The program closes the number it handed us and could now reuse it for a data file.
    drop(write_end);
    sink.write(Level::Error, format_args!("late"));

    assert!(drain(&mut read_end).ends_with("error: late\n"));
}

#[test]
fn a_descriptor_wins_over_a_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log");
    let (mut read_end, write_end) = pipe();

    let sink = Sink::from_env(&env(&[
        (ENV_LOG_FD, write_end.as_raw_fd().to_string()),
        (ENV_LOG, path.to_string_lossy().into_owned()),
    ]))
    .unwrap();
    sink.write(Level::Error, format_args!("to the descriptor"));

    assert!(drain(&mut read_end).ends_with("error: to the descriptor\n"));
    assert!(!path.exists(), "the path was opened as well");
}

#[test]
fn a_descriptor_that_is_not_open_falls_back_to_the_path_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log");

    let sink = Sink::from_env(&env(&[
        (ENV_LOG_FD, "4096".to_owned()),
        (ENV_LOG, path.to_string_lossy().into_owned()),
    ]))
    .unwrap();
    sink.write(Level::Error, format_args!("the line after it"));

    let written = std::fs::read_to_string(&path).unwrap();
    let mut lines = written.lines();
    assert!(
        lines
            .next()
            .unwrap()
            .ends_with("LIBTIMEWARP_LOG_FD: not an open descriptor")
    );
    assert!(lines.next().unwrap().ends_with("error: the line after it"));
}

#[test]
fn a_path_is_appended_to_and_created_private() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log");
    let vars = [(ENV_LOG, path.to_string_lossy().into_owned())];

    Sink::from_env(&env(&vars))
        .unwrap()
        .write(Level::Error, format_args!("first"));
    Sink::from_env(&env(&vars))
        .unwrap()
        .write(Level::Error, format_args!("second"));

    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written.lines().count(), 2, "{written}");
    assert!(written.contains("first") && written.contains("second"));

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{mode:o}");
}

#[test]
fn a_path_that_is_a_symlink_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let link = dir.path().join("link");
    std::fs::write(&target, "").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    assert!(Sink::from_env(&env(&[(ENV_LOG, link.to_string_lossy().into_owned())])).is_none());
}

#[test]
fn a_long_message_is_cut_at_the_cap() {
    let mut line = Line::new();
    for _ in 0..line::CAPACITY {
        let _ = write!(line, "x");
    }

    let bytes = line.finish();
    assert_eq!(bytes.len(), line::CAPACITY);
    assert_eq!(bytes.last(), Some(&b'\n'));
}

#[test]
fn a_cut_never_lands_inside_a_character() {
    let mut line = Line::new();
    // Two bytes each, so the cap falls in the middle of one of them.
    for _ in 0..line::CAPACITY {
        let _ = write!(line, "é");
    }

    let bytes = line.finish();
    assert_eq!(bytes.len(), line::CAPACITY - 1);
    assert_eq!(bytes.last(), Some(&b'\n'));
    assert!(std::str::from_utf8(bytes).is_ok());
}

#[test]
fn a_line_that_fits_is_left_whole() {
    let mut line = Line::new();
    let _ = write!(line, "one {} three", 2);
    assert_eq!(line.text(), "one 2 three");
    assert_eq!(line.finish(), b"one 2 three\n");
}

#[test]
fn an_incident_is_reported_once_per_process() {
    let once = OncePerIncident::new();
    assert!(once.claim(7));
    assert!(!once.claim(7));
    assert!(!once.claim(7));

    // A different incident is its own line, and the one before it does not come back.
    assert!(once.claim(9));
    assert!(!once.claim(9));
    assert!(once.claim(7));
}

#[test]
fn the_word_an_incident_record_starts_at_is_not_an_incident() {
    assert!(!OncePerIncident::new().claim(0));
}
