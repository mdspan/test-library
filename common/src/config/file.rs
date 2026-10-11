//! The configuration file: a TOML document holding the same settings as the environment, read
//! underneath it.

use std::borrow::Cow;
use std::ffi::OsString;
use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use super::{
    ENV_BOOTTIME_RATE, ENV_BOOTTIME_SHIFT, ENV_BOOTTIME_TIMEBASE, ENV_CPU_RATE, ENV_EMULATE_VDSO,
    ENV_FAIL_CLOSED, ENV_HOOK, ENV_KEEPER, ENV_LOG, ENV_LOG_FD, ENV_LOG_LEVEL, ENV_MONOTONIC_RATE,
    ENV_MONOTONIC_SHIFT, ENV_MONOTONIC_TIMEBASE, ENV_ONLY_CMDS, ENV_RATE, ENV_REALTIME_RATE,
    ENV_REALTIME_SHIFT, ENV_REALTIME_TIMEBASE, ENV_SESSION_DIR, ENV_SHIFT, ENV_SKIP_CMDS, ENV_TIME,
    EnvLookup,
};

/// Names the configuration file to read, in place of the user's and the system's.
pub const ENV_CONFIG: &str = "LIBTIMEWARP_CONFIG";

/// The user's file, relative to `$XDG_CONFIG_HOME`.
pub const USER_FILE: &str = "libtimewarp/config.toml";

/// Where `$XDG_CONFIG_HOME` is when it is unset, relative to `$HOME`.
const CONFIG_HOME_UNDER_HOME: &str = ".config";

/// The system-wide file, read when the user has no file of their own.
pub const SYSTEM_FILE: &str = "/etc/libtimewarp/config.toml";

/// Which TOML values a setting takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A string, passed on as it is.
    Text,
    /// A string, or a number of seconds.
    Duration,
    /// A number, or a string holding one.
    Rate,
    /// A string, or an offset date-time.
    Time,
    /// A boolean.
    Flag,
    /// An array of strings, none holding a comma.
    List,
    /// An integer.
    Descriptor,
}

/// Where each setting sits: its table, `None` for the top level, its key, the variable it stands
/// for, and the values it takes.
const KEYS: &[(Option<&str>, &str, &str, Kind)] = &[
    (None, "session_dir", ENV_SESSION_DIR, Kind::Text),
    (None, "keeper", ENV_KEEPER, Kind::Text),
    (None, "hook", ENV_HOOK, Kind::Text),
    (None, "time", ENV_TIME, Kind::Time),
    (None, "shift", ENV_SHIFT, Kind::Duration),
    (None, "rate", ENV_RATE, Kind::Rate),
    (None, "emulate_vdso", ENV_EMULATE_VDSO, Kind::Flag),
    (None, "fail_closed", ENV_FAIL_CLOSED, Kind::Flag),
    (None, "skip_cmds", ENV_SKIP_CMDS, Kind::List),
    (None, "only_cmds", ENV_ONLY_CMDS, Kind::List),
    (
        Some("realtime"),
        "shift",
        ENV_REALTIME_SHIFT,
        Kind::Duration,
    ),
    (Some("realtime"), "rate", ENV_REALTIME_RATE, Kind::Rate),
    (
        Some("realtime"),
        "timebase",
        ENV_REALTIME_TIMEBASE,
        Kind::Text,
    ),
    (
        Some("monotonic"),
        "shift",
        ENV_MONOTONIC_SHIFT,
        Kind::Duration,
    ),
    (Some("monotonic"), "rate", ENV_MONOTONIC_RATE, Kind::Rate),
    (
        Some("monotonic"),
        "timebase",
        ENV_MONOTONIC_TIMEBASE,
        Kind::Text,
    ),
    (
        Some("boottime"),
        "shift",
        ENV_BOOTTIME_SHIFT,
        Kind::Duration,
    ),
    (Some("boottime"), "rate", ENV_BOOTTIME_RATE, Kind::Rate),
    (
        Some("boottime"),
        "timebase",
        ENV_BOOTTIME_TIMEBASE,
        Kind::Text,
    ),
    (Some("cpu"), "rate", ENV_CPU_RATE, Kind::Rate),
    (Some("log"), "file", ENV_LOG, Kind::Text),
    (Some("log"), "fd", ENV_LOG_FD, Kind::Descriptor),
    (Some("log"), "level", ENV_LOG_LEVEL, Kind::Text),
];

/// What is wrong with a configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileReason {
    /// It exists, or is named, and cannot be read.
    Unreadable,
    /// It is not a TOML document; the line, numbered from 1, where that shows.
    Syntax(u32),
    /// A key or table, on this line, that is not a setting.
    UnknownKey(u32),
    /// A value, on this line, of a type its setting does not take.
    WrongType(u32),
}

impl fmt::Display for FileReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable => f.write_str("cannot be read"),
            Self::Syntax(line) => write!(f, "line {line}: not valid TOML"),
            Self::UnknownKey(line) => write!(f, "line {line}: not a setting"),
            Self::WrongType(line) => write!(f, "line {line}: the wrong type for this setting"),
        }
    }
}

/// A configuration file the library could not use. Like a bad value, it is logged, and by default
/// the process runs on genuine time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    /// The file.
    pub path: PathBuf,
    /// What is wrong with it.
    pub reason: FileReason,
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.reason)
    }
}

impl std::error::Error for FileError {}

/// The settings of the configuration file in force, by the variable each stands for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigFile {
    entries: Vec<(&'static str, OsString)>,
}

impl ConfigFile {
    /// Reads the file `env` points at: the one [`ENV_CONFIG`] names, or else the first that exists
    /// of [`USER_FILE`] under `$XDG_CONFIG_HOME`, `$HOME/.config` when that is unset, and
    /// [`SYSTEM_FILE`]. Finding neither is no file at all.
    ///
    /// # Errors
    /// [`FileError`] when the named file cannot be read, a default file exists and cannot be read,
    /// or the file in force is not one [`ConfigFile::parse`] takes.
    pub fn load(env: &impl EnvLookup) -> Result<Self, FileError> {
        let fail = |path: &Path, reason| FileError {
            path: path.to_path_buf(),
            reason,
        };
        if let Some(named) = env.nonempty(ENV_CONFIG) {
            let path = PathBuf::from(named);
            let text = std::fs::read(&path).map_err(|_| fail(&path, FileReason::Unreadable))?;
            return Self::parse(&text).map_err(|why| fail(&path, why));
        }
        let user = env
            .nonempty("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                env.nonempty("HOME")
                    .map(|home| Path::new(&home).join(CONFIG_HOME_UNDER_HOME))
            })
            .map(|base| base.join(USER_FILE));
        for path in user.into_iter().chain([PathBuf::from(SYSTEM_FILE)]) {
            if let Some(text) = read_if_present(&path).map_err(|why| fail(&path, why))? {
                return Self::parse(&text).map_err(|why| fail(&path, why));
            }
        }
        Ok(Self::default())
    }

    /// Parses a TOML document.
    ///
    /// Each key names a setting, at the top level or in the table of its family or of `log`, and
    /// holds a value of a type that setting takes. A number given for a duration is seconds, a
    /// boolean is a flag's `1` or `0`, and an array of commands is the comma-separated list.
    ///
    /// # Errors
    /// [`FileReason`] with the line of the first thing that is none of those.
    pub fn parse(text: &[u8]) -> Result<Self, FileReason> {
        let text = std::str::from_utf8(text)
            .map_err(|why| FileReason::Syntax(line_of(text, why.valid_up_to())))?;
        let document = DeTable::parse(text).map_err(|why| {
            FileReason::Syntax(
                why.span()
                    .map_or(1, |span| line_of(text.as_bytes(), span.start)),
            )
        })?;
        let mut entries = Vec::new();
        for (key, value) in document.get_ref() {
            match value.get_ref() {
                DeValue::Table(table) if is_section(key.get_ref()) => {
                    for (inner, value) in table {
                        entries.push(setting(text, Some(key.get_ref()), inner, value)?);
                    }
                }
                _ => entries.push(setting(text, None, key, value)?),
            }
        }
        Ok(Self { entries })
    }

    /// The value the file gives the variable `name`, or `None` when it gives none.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<OsString> {
        self.entries
            .iter()
            .find(|(set, _)| *set == name)
            .map(|(_, value)| value.clone())
    }
}

/// Whether `key` names a table of settings.
fn is_section(key: &str) -> bool {
    KEYS.iter().any(|(table, _, _, _)| *table == Some(key))
}

/// The variable `key` in `table` stands for, and the value it gives that variable.
fn setting(
    text: &str,
    table: Option<&str>,
    key: &Spanned<Cow<'_, str>>,
    value: &Spanned<DeValue<'_>>,
) -> Result<(&'static str, OsString), FileReason> {
    let &(_, _, variable, kind) = KEYS
        .iter()
        .find(|(in_table, name, _, _)| *in_table == table && *name == key.get_ref())
        .ok_or_else(|| FileReason::UnknownKey(line_of(text.as_bytes(), key.span().start)))?;
    let shown = convert(kind, value.get_ref())
        .ok_or_else(|| FileReason::WrongType(line_of(text.as_bytes(), value.span().start)))?;
    Ok((variable, shown.into()))
}

/// A value as the environment would spell it, or `None` when `kind` does not take its type.
fn convert(kind: Kind, value: &DeValue<'_>) -> Option<String> {
    match (kind, value) {
        (Kind::Text | Kind::Duration | Kind::Rate | Kind::Time, DeValue::String(text)) => {
            Some(text.to_string())
        }
        (Kind::Duration | Kind::Rate | Kind::Descriptor, DeValue::Integer(number)) => {
            i64::from_str_radix(number.as_str(), number.radix())
                .ok()
                .map(|number| number.to_string())
        }
        (Kind::Duration | Kind::Rate, DeValue::Float(number)) => Some(number.as_str().to_owned()),
        // A date-time with no offset names no instant until a time zone is picked for it.
        (Kind::Time, DeValue::Datetime(when)) if when.date.is_some() && when.offset.is_some() => {
            Some(when.to_string())
        }
        (Kind::Flag, DeValue::Boolean(on)) => Some(if *on { "1" } else { "0" }.to_owned()),
        (Kind::List, DeValue::Array(items)) => {
            let names = items
                .iter()
                .map(|item| item.get_ref().as_str().filter(|name| !name.contains(',')))
                .collect::<Option<Vec<_>>>()?;
            Some(names.join(","))
        }
        _ => None,
    }
}

/// The line, numbered from 1, that byte `offset` of `text` is on.
fn line_of(text: &[u8], offset: usize) -> u32 {
    let lines = text
        .get(..offset)
        .unwrap_or(text)
        .split(|&byte| byte == b'\n');
    u32::try_from(lines.count()).unwrap_or(u32::MAX)
}

/// A file's bytes, or `None` when there is no file at `path`.
fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, FileReason> {
    match std::fs::read(path) {
        Ok(text) => Ok(Some(text)),
        Err(why) if why.kind() == ErrorKind::NotFound => Ok(None),
        Err(_) => Err(FileReason::Unreadable),
    }
}

/// The environment over the configuration file. A variable set in the environment hides the file's
/// setting for it, even when the value there is empty and so reads as unset.
#[derive(Debug, Clone)]
pub struct Settings<E> {
    env: E,
    file: ConfigFile,
}

impl<E: EnvLookup> Settings<E> {
    /// `env` over the file it points at.
    ///
    /// # Errors
    /// The [`FileError`] from [`ConfigFile::load`].
    pub fn load(env: E) -> Result<Self, FileError> {
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

    fn parsed(text: &str) -> ConfigFile {
        ConfigFile::parse(text.as_bytes()).unwrap_or_else(|why| panic!("{text:?}: {why}"))
    }

    #[test]
    fn keys_stand_for_the_variables_of_the_same_meaning() {
        let file = parsed(
            r#"
            shift = "-2h"
            rate = 10
            skip_cmds = ["curl", "git"]
            fail_closed = true

            [realtime]
            rate = 0
            timebase = "CLOCK_REALTIME"

            [cpu]
            rate = 0.5

            [log]
            fd = 2
            level = "debug"
            "#,
        );
        assert_eq!(file.get(ENV_SHIFT), Some("-2h".into()));
        assert_eq!(file.get(ENV_RATE), Some("10".into()));
        assert_eq!(file.get(ENV_SKIP_CMDS), Some("curl,git".into()));
        assert_eq!(file.get(ENV_FAIL_CLOSED), Some("1".into()));
        assert_eq!(file.get(ENV_REALTIME_RATE), Some("0".into()));
        assert_eq!(
            file.get(ENV_REALTIME_TIMEBASE),
            Some("CLOCK_REALTIME".into())
        );
        assert_eq!(file.get(ENV_CPU_RATE), Some("0.5".into()));
        assert_eq!(file.get(ENV_LOG_FD), Some("2".into()));
        assert_eq!(file.get(ENV_LOG_LEVEL), Some("debug".into()));
        assert_eq!(file.get(ENV_MONOTONIC_RATE), None);
    }

    #[test]
    fn dotted_keys_reach_the_same_settings_as_tables() {
        let file = parsed("monotonic.shift = \"1h\"\nlog.file = \"/tmp/tw.log\"\n");
        assert_eq!(file.get(ENV_MONOTONIC_SHIFT), Some("1h".into()));
        assert_eq!(file.get(ENV_LOG), Some("/tmp/tw.log".into()));
    }

    #[test]
    fn a_number_for_a_duration_is_seconds() {
        assert_eq!(parsed("shift = -7200").get(ENV_SHIFT), Some("-7200".into()));
        assert_eq!(parsed("shift = 1.5").get(ENV_SHIFT), Some("1.5".into()));
        assert_eq!(parsed("shift = 0x10").get(ENV_SHIFT), Some("16".into()));
    }

    #[test]
    fn a_time_is_an_offset_date_time_or_a_string() {
        assert_eq!(
            parsed("time = 2020-01-01T00:00:00Z").get(ENV_TIME),
            Some("2020-01-01T00:00:00Z".into())
        );
        assert_eq!(
            parsed("time = 2020-01-01T02:00:00+02:00").get(ENV_TIME),
            Some("2020-01-01T02:00:00+02:00".into())
        );
        assert_eq!(
            parsed("time = \"@1577836800\"").get(ENV_TIME),
            Some("@1577836800".into())
        );
    }

    #[test]
    fn a_flag_off_is_zero() {
        assert_eq!(
            parsed("emulate_vdso = false").get(ENV_EMULATE_VDSO),
            Some("0".into())
        );
    }

    #[test]
    fn an_empty_file_sets_nothing() {
        assert_eq!(parsed(""), ConfigFile::default());
        assert_eq!(parsed("# only a comment\n"), ConfigFile::default());
    }

    #[test]
    fn a_document_that_is_not_toml_names_its_line() {
        for (text, line) in [
            ("rate = 1\nshift = -2h\n", 2),
            ("rate = 1\nrate = 2\n", 2),
            ("\n\n[log\n", 3),
        ] {
            assert_eq!(
                ConfigFile::parse(text.as_bytes()),
                Err(FileReason::Syntax(line)),
                "{text:?}"
            );
        }
        assert_eq!(
            ConfigFile::parse(b"rate = 1\nshift = \"\xff\"\n"),
            Err(FileReason::Syntax(2))
        );
    }

    #[test]
    fn a_key_that_is_not_a_setting_names_its_line() {
        for (text, line) in [
            ("rate = 1\nshfit = \"1h\"\n", 2),
            ("[cpu]\nshift = \"1h\"\n", 2),
            ("[realtime]\nrate = 1\n[clocks]\nrate = 1\n", 3),
            ("timebase = \"CLOCK_REALTIME\"\n", 1),
            ("config = \"/etc/x\"\n", 1),
            ("[log]\n[log.inner]\nfd = 2\n", 2),
            ("realtime = 1\n", 1),
            ("LIBTIMEWARP_RATE=1\n", 1),
        ] {
            assert_eq!(
                ConfigFile::parse(text.as_bytes()),
                Err(FileReason::UnknownKey(line)),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_value_of_the_wrong_type_names_its_line() {
        for (text, line) in [
            ("shift = true\n", 1),
            ("rate = \"1\"\nemulate_vdso = 1\n", 2),
            ("skip_cmds = \"curl\"\n", 1),
            ("skip_cmds = [\"curl,git\"]\n", 1),
            ("skip_cmds = [\"curl\", 2]\n", 1),
            ("time = 2020-01-01T00:00:00\n", 1),
            ("time = 2020-01-01\n", 1),
            ("[log]\nfd = \"2\"\n", 2),
            ("keeper = 1\n", 1),
        ] {
            assert_eq!(
                ConfigFile::parse(text.as_bytes()),
                Err(FileReason::WrongType(line)),
                "{text:?}"
            );
        }
    }

    #[test]
    fn the_environment_wins_over_the_file() {
        let env = |name: &str| (name == ENV_SHIFT).then(|| "1h".into());
        let settings = Settings::new(env, parsed("shift = \"-2h\"\nrate = 3\n"));
        assert_eq!(settings.get(ENV_SHIFT), Some("1h".into()));
        assert_eq!(settings.get(ENV_RATE), Some("3".into()));
    }

    #[test]
    fn an_empty_variable_hides_the_files_setting() {
        let env = |name: &str| (name == ENV_SHIFT).then(|| "".into());
        let settings = Settings::new(env, parsed("shift = \"-2h\"\n"));
        assert_eq!(settings.nonempty(ENV_SHIFT), None);
    }

    #[test]
    fn the_named_file_is_read_and_a_missing_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timewarp.toml");
        std::fs::write(&path, "rate = 4\n").unwrap();
        let env = move |name: &str| (name == ENV_CONFIG).then(|| path.clone().into());
        assert_eq!(
            ConfigFile::load(&env).map(|file| file.get(ENV_RATE)),
            Ok(Some("4".into()))
        );

        let missing = dir.path().join("absent.toml");
        let named = missing.clone();
        let env = move |name: &str| (name == ENV_CONFIG).then(|| named.clone().into());
        let error = ConfigFile::load(&env).unwrap_err();
        assert_eq!(
            error,
            FileError {
                path: missing.clone(),
                reason: FileReason::Unreadable
            }
        );
        assert_eq!(
            error.to_string(),
            format!("{}: cannot be read", missing.display())
        );
    }

    #[test]
    fn the_user_file_is_under_the_config_home() {
        let dir = tempfile::tempdir().unwrap();
        let config_home = dir.path().join("xdg");
        std::fs::create_dir_all(config_home.join("libtimewarp")).unwrap();
        std::fs::write(config_home.join(USER_FILE), "rate = 5\n").unwrap();
        let env = move |name: &str| (name == "XDG_CONFIG_HOME").then(|| config_home.clone().into());
        assert_eq!(
            ConfigFile::load(&env).map(|file| file.get(ENV_RATE)),
            Ok(Some("5".into()))
        );
    }

    #[test]
    fn without_a_config_home_the_user_file_is_under_home() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".config").join(USER_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "rate = 6\n[log]\nlevel = 3\n").unwrap();
        let at = home.path().to_path_buf();
        let env = move |name: &str| (name == "HOME").then(|| at.clone().into());
        let error = ConfigFile::load(&env).unwrap_err();
        assert_eq!(error.path, path);
        assert_eq!(error.reason, FileReason::WrongType(3));
        assert!(
            error
                .to_string()
                .ends_with("config.toml: line 3: the wrong type for this setting"),
            "{error}"
        );
    }
}
