//! The configuration file: the same variables as the environment, one `NAME=VALUE` per line, read
//! underneath it.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use super::{BadValue, EnvLookup, Reason, SETTINGS};

/// Names the configuration file to read, in place of [`USER_FILE`] and [`SYSTEM_FILE`].
pub const ENV_CONFIG: &str = "LIBTIMEWARP_CONFIG";

/// The per-user file, under `$HOME`, read when [`ENV_CONFIG`] is unset.
pub const USER_FILE: &str = ".libtimewarprc";

/// The system-wide file, read when [`ENV_CONFIG`] is unset and the user has no file of their own.
pub const SYSTEM_FILE: &str = "/etc/libtimewarprc";

/// How a bad user file names itself, since its path is not known until run time.
const USER_FILE_SHOWN: &str = "~/.libtimewarprc";

/// The entries of the configuration file in force, or none when there is no file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigFile {
    entries: Vec<(&'static str, OsString)>,
}

impl ConfigFile {
    /// Reads the file `env` points at: the one [`ENV_CONFIG`] names, or else the first of
    /// [`USER_FILE`] under `$HOME` and [`SYSTEM_FILE`] that exists. Finding none of the defaults is no
    /// file at all.
    ///
    /// # Errors
    /// [`BadValue`] when the named file cannot be read, a default file exists and cannot be read,
    /// or a line is not a setting the file may hold.
    pub fn load(env: &impl EnvLookup) -> Result<Self, BadValue> {
        if let Some(named) = env.nonempty(ENV_CONFIG) {
            let text =
                std::fs::read(&named).map_err(|_| BadValue::new(ENV_CONFIG, Reason::Unreadable))?;
            return Self::parse(&text).map_err(|line| BadValue::new(ENV_CONFIG, line));
        }
        let user = env
            .nonempty("HOME")
            .map(|home| (PathBuf::from(home).join(USER_FILE), USER_FILE_SHOWN));
        let system = (PathBuf::from(SYSTEM_FILE), SYSTEM_FILE);
        for (path, shown) in user.into_iter().chain([system]) {
            if let Some(text) = read_if_present(&path).map_err(|why| BadValue::new(shown, why))? {
                return Self::parse(&text).map_err(|line| BadValue::new(shown, line));
            }
        }
        Ok(Self::default())
    }

    /// Parses the text of a file.
    ///
    /// A line is blank, a comment starting with `#`, or `NAME=VALUE` with `NAME` one of the
    /// variables the library reads. Space around the name and the value is dropped, and nothing
    /// else is: no quotes, no escapes, no comment after a value.
    ///
    /// # Errors
    /// [`Reason::Line`] with the number of the first line that is none of those, or that names a
    /// variable an earlier line already set.
    pub fn parse(text: &[u8]) -> Result<Self, Reason> {
        let mut entries: Vec<(&'static str, OsString)> = Vec::new();
        for (index, line) in text.split(|&byte| byte == b'\n').enumerate() {
            let bad =
                || Reason::Line(u32::try_from(index).map_or(u32::MAX, |n| n.saturating_add(1)));
            let line = line.trim_ascii();
            if line.is_empty() || line.starts_with(b"#") {
                continue;
            }
            let equals = line.iter().position(|&byte| byte == b'=').ok_or_else(bad)?;
            let (name, value) = line.split_at(equals);
            let value = value.get(1..).unwrap_or_default().trim_ascii();
            let name = SETTINGS
                .iter()
                .copied()
                .find(|known| known.as_bytes() == name.trim_ascii())
                .ok_or_else(bad)?;
            if entries.iter().any(|(set, _)| *set == name) {
                return Err(bad());
            }
            entries.push((name, OsString::from_vec(value.to_vec())));
        }
        Ok(Self { entries })
    }

    /// The value the file gives `name`, or `None` when it gives none.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<OsString> {
        self.entries
            .iter()
            .find(|(set, _)| *set == name)
            .map(|(_, value)| value.clone())
    }
}

/// A file's bytes, or `None` when there is no file at `path`.
fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, Reason> {
    match std::fs::read(path) {
        Ok(text) => Ok(Some(text)),
        Err(why) if why.kind() == ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Reason::Unreadable),
    }
}

/// The environment over the configuration file. A variable set in the environment hides the file's
/// entry for it, even when the value there is empty and so reads as unset.
#[derive(Debug, Clone)]
pub struct Settings<E> {
    env: E,
    file: ConfigFile,
}

impl<E: EnvLookup> Settings<E> {
    /// `env` over the file it points at.
    ///
    /// # Errors
    /// The [`BadValue`] from [`ConfigFile::load`].
    pub fn load(env: E) -> Result<Self, BadValue> {
        let file = ConfigFile::load(&env)?;
        Ok(Self { env, file })
    }

    /// `env` over `file`.
    pub const fn new(env: E, file: ConfigFile) -> Self {
        Self { env, file }
    }
}

impl<E: EnvLookup> EnvLookup for Settings<E> {
    fn get(&self, name: &str) -> Option<OsString> {
        self.env.get(name).or_else(|| self.file.get(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ENV_RATE, ENV_SHARED_FILE, ENV_SHIFT, ENV_SKIP_CMDS};

    fn parsed(text: &str) -> ConfigFile {
        ConfigFile::parse(text.as_bytes()).unwrap_or_else(|why| panic!("{text:?}: {why}"))
    }

    #[test]
    fn a_line_sets_the_variable_it_names() {
        let file = parsed("LIBTIMEWARP_SHIFT=-2h\nLIBTIMEWARP_RATE=10\n");
        assert_eq!(file.get(ENV_SHIFT), Some("-2h".into()));
        assert_eq!(file.get(ENV_RATE), Some("10".into()));
        assert_eq!(file.get(ENV_SKIP_CMDS), None);
    }

    #[test]
    fn comments_blank_lines_and_surrounding_space_are_ignored() {
        let file = parsed("# a comment\n\n   \n  LIBTIMEWARP_SHIFT = -2h \r\n\t# another\n");
        assert_eq!(file.get(ENV_SHIFT), Some("-2h".into()));
    }

    #[test]
    fn a_value_is_kept_verbatim_past_its_first_equals_sign() {
        let file = parsed("LIBTIMEWARP_SKIP_CMDS=a=b,\"c\" # d\n");
        assert_eq!(file.get(ENV_SKIP_CMDS), Some("a=b,\"c\" # d".into()));
    }

    #[test]
    fn an_empty_file_sets_nothing() {
        assert_eq!(parsed(""), ConfigFile::default());
    }

    #[test]
    fn a_bad_line_is_named_by_its_number() {
        let cases = [
            ("LIBTIMEWARP_SHIFT=1h\nno equals sign\n", 2),
            ("LIBTIMEWARP_SHFIT=1h\n", 1),
            ("\n\nLIBTIMEWARP_RATE=1\nLIBTIMEWARP_RATE=2\n", 4),
            ("export LIBTIMEWARP_RATE=1\n", 1),
            ("=1\n", 1),
        ];
        for (text, line) in cases {
            assert_eq!(
                ConfigFile::parse(text.as_bytes()),
                Err(Reason::Line(line)),
                "{text:?}"
            );
        }
    }

    #[test]
    fn variables_the_library_sets_or_that_choose_the_file_are_refused() {
        for name in [ENV_SHARED_FILE, ENV_CONFIG, "LD_PRELOAD", "HOME"] {
            let text = format!("{name}=x\n");
            assert_eq!(
                ConfigFile::parse(text.as_bytes()),
                Err(Reason::Line(1)),
                "{name}"
            );
        }
    }

    #[test]
    fn the_environment_wins_over_the_file() {
        let env = |name: &str| (name == ENV_SHIFT).then(|| "1h".into());
        let settings = Settings::new(env, parsed("LIBTIMEWARP_SHIFT=-2h\nLIBTIMEWARP_RATE=3\n"));
        assert_eq!(settings.get(ENV_SHIFT), Some("1h".into()));
        assert_eq!(settings.get(ENV_RATE), Some("3".into()));
    }

    #[test]
    fn an_empty_variable_hides_the_files_entry() {
        let env = |name: &str| (name == ENV_SHIFT).then(|| "".into());
        let settings = Settings::new(env, parsed("LIBTIMEWARP_SHIFT=-2h\n"));
        assert_eq!(settings.nonempty(ENV_SHIFT), None);
    }

    #[test]
    fn the_named_file_is_read_and_a_missing_one_is_a_bad_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rc");
        std::fs::write(&path, "LIBTIMEWARP_RATE=4\n").unwrap();
        let env = move |name: &str| (name == ENV_CONFIG).then(|| path.clone().into());
        assert_eq!(
            ConfigFile::load(&env).map(|file| file.get(ENV_RATE)),
            Ok(Some("4".into()))
        );

        let missing = dir.path().join("absent");
        let env = move |name: &str| (name == ENV_CONFIG).then(|| missing.clone().into());
        assert_eq!(
            ConfigFile::load(&env),
            Err(BadValue::new(ENV_CONFIG, Reason::Unreadable))
        );
    }

    #[test]
    fn the_user_file_is_read_from_home_and_a_bad_one_names_itself() {
        let home = tempfile::tempdir().unwrap();
        let at = home.path().to_path_buf();
        let env = move |name: &str| (name == "HOME").then(|| at.clone().into());

        std::fs::write(home.path().join(USER_FILE), "LIBTIMEWARP_RATE=5\n").unwrap();
        assert_eq!(
            ConfigFile::load(&env).map(|file| file.get(ENV_RATE)),
            Ok(Some("5".into()))
        );

        std::fs::write(home.path().join(USER_FILE), "LIBTIMEWARP_RATE\n").unwrap();
        let bad = ConfigFile::load(&env).unwrap_err();
        assert_eq!(bad, BadValue::new(USER_FILE_SHOWN, Reason::Line(1)));
        assert_eq!(
            bad.to_string(),
            "~/.libtimewarprc: line 1 is not a setting the file may hold"
        );
    }
}
