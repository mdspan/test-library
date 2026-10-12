//! The session directory and the sessions in it, told apart by the creator pid and creation time
//! their headers carry.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};

use common::config::{ProcessEnv, Settings};
use common::session::naming::{SESSION_SUFFIX, session_dir_from_env};
use controller::{Controller, Info};

use crate::failure::Failure;

/// The directory the library puts sessions in: the environment over the configuration file, or the
/// environment alone when the file is refused.
pub(crate) fn session_dir() -> PathBuf {
    Settings::load(ProcessEnv).map_or_else(
        |_| session_dir_from_env(&ProcessEnv),
        |settings| session_dir_from_env(&settings),
    )
}

/// A session file in the directory, and its header.
#[derive(Debug)]
pub(crate) struct Found {
    pub(crate) path: PathBuf,
    pub(crate) info: Info,
}

/// Every `*.session` file in `directory` that attaches, newest first. Files created in the same
/// nanosecond stay in name order. A directory that cannot be read has none.
pub(crate) fn sessions(directory: &Path) -> Vec<Found> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.as_encoded_bytes().ends_with(SESSION_SUFFIX.as_bytes()))
        })
        .collect();
    paths.sort();
    let mut found: Vec<Found> = paths
        .into_iter()
        .filter_map(|path| {
            let info = Controller::attach(&path).ok()?.info();
            Some(Found { path, info })
        })
        .collect();
    found.sort_by_key(|one| Reverse(one.info.creation_time));
    found
}

/// The one session in `directory` a command acts on: those `creator` created, if given, then the
/// newest of them when `newest` is set or only one is left.
pub(crate) fn select(
    directory: &Path,
    creator: Option<u32>,
    newest: bool,
) -> Result<Found, Failure> {
    let mut found = sessions(directory);
    if let Some(pid) = creator {
        found.retain(|one| one.info.creator_pid == pid);
    }
    if found.len() > 1 && !newest {
        let listed = found
            .iter()
            .map(|one| {
                format!(
                    "\n  {} (creator pid {}, created {} ns)",
                    one.path.display(),
                    one.info.creator_pid,
                    one.info.creation_time.0
                )
            })
            .collect::<Vec<_>>()
            .concat();
        return Err(Failure::Other(format!(
            "{} sessions in {}; name one with --session, --creator or --newest:{listed}",
            found.len(),
            directory.display()
        )));
    }
    found
        .into_iter()
        .next()
        .ok_or_else(|| Failure::Other(format!("no session in {}", directory.display())))
}
