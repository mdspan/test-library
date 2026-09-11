//! A suspected writer death writes one `warn` line per process per incident.
//!
//! Installing a destination is a whole-process thing, and so is the record of what this process
//! has already reported, so this check lives in a test binary of its own.
//!
//! Creating a session reads a genuine clock and the system's randomness, so this needs a
//! filled-in platform arm.
#![cfg(any(target_os = "linux", target_os = "freebsd"))]

use std::sync::atomic::Ordering;

use common::config::{ENV_LOG, ENV_LOG_LEVEL};
use common::log::{self, Sink};
use common::session::layout::{Header, Params, WriterState};
use common::session::{self, Setup};
use common::sync::read;

/// The words the line about a suspected death carries, and nothing else in the library writes.
const SUSPICION: &str = "left a write section open";

#[test]
fn a_suspected_death_is_reported_once_per_process_per_incident() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let session = session::create(&Setup::plain(dir.path().join("libtimewarp"))).unwrap();
    let header = session.header();

    let path = log.clone().into_os_string();
    let level = std::ffi::OsString::from("warn");
    log::install(
        Sink::from_env(&|name: &str| match name {
            ENV_LOG => Some(path.clone()),
            ENV_LOG_LEVEL => Some(level.clone()),
            _ => None,
        })
        .unwrap(),
    );

    // A writer that died with the live words still whole, twice over: one incident, however many
    // reads run into it.
    abandon(header, 1);
    read(header, Params::load);
    read(header, Params::load);
    assert_eq!(mentions(&log), 1, "one line per incident, not one per read");

    // A section that opened and was abandoned again is another incident, and gets its own line.
    abandon(header, 3);
    read(header, Params::load);
    assert_eq!(mentions(&log), 2);

    // The pid the writer state word records is what the line names.
    let written = std::fs::read_to_string(&log).unwrap();
    assert!(written.contains("pid 4321"), "{written}");
    assert!(written.contains("warn"), "{written}");
}

/// The seqlock as a writer that died between W3 and W7 leaves it: counter odd, owner recorded,
/// shadow-complete clear.
fn abandon(header: &Header, counter: u64) {
    header
        .seqlock
        .writer_state
        .store(WriterState::writing(4321).word(), Ordering::Release);
    header.seqlock.sequence.store(counter, Ordering::Release);
}

fn mentions(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(SUSPICION))
        .count()
}
