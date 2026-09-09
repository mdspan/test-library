//! Where a session file lives and what it is called.

use std::path::{Path, PathBuf};

use crate::config::{ENV_SESSION_DIR, ENV_XDG_RUNTIME_DIR, EnvLookup, ProcessEnv};

/// What a finished session file is called.
pub const SESSION_SUFFIX: &str = ".session";

/// What a session file is called while it is still being built.
pub const TMP_SUFFIX: &str = ".session.tmp";

/// What the write lock's file is called.
pub const LOCK_SUFFIX: &str = ".lock";

/// The directory holding session files: the first rule that applies.
#[must_use]
pub fn session_dir(env: &impl EnvLookup, euid: u32, tmp: &Path) -> PathBuf {
    if let Some(dir) = env.nonempty(ENV_SESSION_DIR) {
        return PathBuf::from(dir).join("libtimewarp");
    }
    if let Some(dir) = env.nonempty(ENV_XDG_RUNTIME_DIR) {
        return PathBuf::from(dir).join("libtimewarp");
    }
    tmp.join(format!("libtimewarp-{euid}"))
}

/// The directory this process would use.
#[must_use]
pub fn session_dir_from_env() -> PathBuf {
    // Safety: `geteuid` reads process state and cannot fail.
    let euid = unsafe { libc::geteuid() };
    session_dir(&ProcessEnv, euid, &std::env::temp_dir())
}

/// The session's name: `pid-<pid>-<uuid>`, which the user does not get to pick. The pid alone
/// would not be unique and a guessable name could be adopted.
#[must_use]
pub fn session_name(pid: u32, random: [u8; 16]) -> String {
    format!("pid-{pid}-{}", uuid_v4(random))
}

/// The path of a session file, and of the temporary name it is built under.
#[must_use]
pub fn session_paths(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
    (
        dir.join(format!("{name}{SESSION_SUFFIX}")),
        dir.join(format!("{name}{TMP_SUFFIX}")),
    )
}

/// The write lock's file, beside the session file it guards: the session's name with
/// [`LOCK_SUFFIX`] in place of [`SESSION_SUFFIX`].
///
/// A path that is not named like a session file, which is what an inherited path may be, keeps its
/// whole name and gains the suffix, so two such paths still get two different lock files.
#[must_use]
pub fn lock_path(session: &Path) -> PathBuf {
    let name = session
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let stem = name.strip_suffix(SESSION_SUFFIX).unwrap_or(&name);
    session.with_file_name(format!("{stem}{LOCK_SUFFIX}"))
}

fn uuid_v4(mut bytes: [u8; 16]) -> String {
    use std::fmt::Write as _;

    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let mut out = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        // Writing into the string keeps the per-byte `String` a `format!` would allocate away.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    struct Map(Vec<(&'static str, &'static str)>);

    impl EnvLookup for Map {
        fn get(&self, name: &str) -> Option<OsString> {
            self.0
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    fn env(pairs: &[(&'static str, &'static str)]) -> Map {
        Map(pairs.to_vec())
    }

    #[test]
    fn the_session_dir_variable_wins() {
        let dir = session_dir(
            &env(&[
                (ENV_SESSION_DIR, "/run/user/1000"),
                (ENV_XDG_RUNTIME_DIR, "/xdg"),
            ]),
            7,
            Path::new("/tmp"),
        );
        assert_eq!(dir, Path::new("/run/user/1000/libtimewarp"));
    }

    #[test]
    fn xdg_runtime_dir_comes_second() {
        let dir = session_dir(&env(&[(ENV_XDG_RUNTIME_DIR, "/xdg")]), 7, Path::new("/tmp"));
        assert_eq!(dir, Path::new("/xdg/libtimewarp"));
    }

    #[test]
    fn the_default_separates_users_by_euid() {
        let dir = session_dir(&env(&[]), 1000, Path::new("/tmp"));
        assert_eq!(dir, Path::new("/tmp/libtimewarp-1000"));
        assert_ne!(dir, session_dir(&env(&[]), 1001, Path::new("/tmp")));
    }

    #[test]
    fn an_empty_variable_is_unset() {
        let dir = session_dir(
            &env(&[(ENV_SESSION_DIR, ""), (ENV_XDG_RUNTIME_DIR, "")]),
            5,
            Path::new("/tmp"),
        );
        assert_eq!(dir, Path::new("/tmp/libtimewarp-5"));
    }

    #[test]
    fn the_name_carries_the_pid_and_a_uuid() {
        let name = session_name(4321, [0xab; 16]);
        assert!(name.starts_with("pid-4321-"), "{name}");
        let uuid = &name["pid-4321-".len()..];
        assert_eq!(uuid.len(), 36);
        assert_eq!(
            uuid.split('-').map(str::len).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert_eq!(uuid.as_bytes()[14], b'4', "version nibble");
        assert!(
            matches!(uuid.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
            "variant nibble"
        );
    }

    #[test]
    fn different_randomness_gives_different_names() {
        assert_ne!(session_name(1, [0; 16]), session_name(1, [1; 16]));
    }

    #[test]
    fn the_lock_file_sits_beside_the_session_file_under_the_same_name() {
        let (path, _) = session_paths(Path::new("/d"), "pid-1-x");
        assert_eq!(lock_path(&path), Path::new("/d/pid-1-x.lock"));
        // Two sessions never share a lock file, and a path that is not named like a session file
        // still gets one of its own.
        assert_ne!(
            lock_path(&path),
            lock_path(&session_paths(Path::new("/d"), "pid-2-x").0)
        );
        assert_eq!(lock_path(Path::new("/d/plain")), Path::new("/d/plain.lock"));
    }

    #[test]
    fn the_temporary_name_sits_beside_the_session_file() {
        let (path, tmp) = session_paths(Path::new("/d"), "pid-1-x");
        assert_eq!(path, Path::new("/d/pid-1-x.session"));
        assert_eq!(tmp, Path::new("/d/pid-1-x.session.tmp"));
        assert_eq!(path.parent(), tmp.parent());
    }
}
