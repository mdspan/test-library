//! The rates a freeze returns to: one file per session, `<state dir>/frozen/<session file name>`,
//! with a `<family>=<rate>` line per remembered family.
//!
//! The controller keeps no memory between runs, so this file is the only place the rate before a
//! freeze survives.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use common::config::{EnvLookup, ProcessEnv};
use common::time::Family;
use controller::{Controller, Transaction};

use crate::change::{Change, Step};
use crate::failure::Failure;
use crate::family;

/// The rates remembered for one session, in the order they were first remembered.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Remembered {
    rates: Vec<(Family, f64)>,
}

impl Remembered {
    /// Reads the text of a state file. A line that is not `<family>=<rate>` is refused with its
    /// number; blank lines are skipped.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut remembered = Self::default();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let entry = line.split_once('=').and_then(|(name, rate)| {
                Some((family::named(name).ok()?, rate.trim().parse().ok()?))
            });
            let Some((family, rate)) = entry else {
                return Err(format!(
                    "line {} is not <family>=<rate>: {line:?}",
                    index + 1
                ));
            };
            remembered.keep(family, rate);
        }
        Ok(remembered)
    }

    /// The text of the state file. `{}` writes the shortest decimal that reads back to the same
    /// `f64`.
    pub(crate) fn render(&self) -> String {
        self.rates
            .iter()
            .fold(String::new(), |mut text, &(family, rate)| {
                text.push_str(family::label(family));
                text.push('=');
                text.push_str(&rate.to_string());
                text.push('\n');
                text
            })
    }

    /// The rate remembered for `family`.
    pub(crate) fn rate(&self, family: Family) -> Option<f64> {
        self.rates
            .iter()
            .find(|&&(known, _)| known == family)
            .map(|&(_, rate)| rate)
    }

    /// Remembers `rate` for `family` unless a rate is already remembered for it. Whether it was
    /// added.
    pub(crate) fn keep(&mut self, family: Family, rate: f64) -> bool {
        let absent = self.rate(family).is_none();
        if absent {
            self.rates.push((family, rate));
        }
        absent
    }

    /// Drops what is remembered for `family`.
    pub(crate) fn forget(&mut self, family: Family) {
        self.rates.retain(|&(known, _)| known != family);
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.rates.is_empty()
    }
}

/// `LIBTIMEWARP_STATE_DIR`, else `$XDG_STATE_HOME/libtimewarp`, else
/// `~/.local/state/libtimewarp`. Empty values count as unset. `None` with none of the three.
pub(crate) fn state_dir(env: &impl EnvLookup) -> Option<PathBuf> {
    if let Some(named) = env.nonempty("LIBTIMEWARP_STATE_DIR") {
        return Some(named.into());
    }
    env.nonempty("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            env.nonempty("HOME")
                .map(|home| Path::new(&home).join(".local/state"))
        })
        .map(|base| base.join("libtimewarp"))
}

/// The state file of one session.
#[derive(Debug)]
pub(crate) struct StateFile {
    path: PathBuf,
}

impl StateFile {
    /// The state file of the session at `session`, under the state directory the process
    /// environment names.
    pub(crate) fn of(session: &Path) -> Result<Self, Failure> {
        let dir = state_dir(&ProcessEnv).ok_or_else(|| {
            Failure::Other(
                "no state directory to remember rates in: set LIBTIMEWARP_STATE_DIR".to_owned(),
            )
        })?;
        Self::under(&dir, session)
    }

    /// The state file of the session at `session`, under `dir`. Keyed by the file name alone, so
    /// every spelling of one session's path finds the same file.
    pub(crate) fn under(dir: &Path, session: &Path) -> Result<Self, Failure> {
        let name = session
            .file_name()
            .ok_or_else(|| Failure::Other(format!("{} does not name a file", session.display())))?;
        Ok(Self {
            path: dir.join("frozen").join(name),
        })
    }

    /// What the file remembers; nothing when there is no file.
    pub(crate) fn load(&self) -> Result<Remembered, Failure> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Remembered::parse(&text)
                .map_err(|why| Failure::Other(format!("{}: {why}", self.path.display()))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Remembered::default()),
            Err(err) => Err(self.failed("reading", &err)),
        }
    }

    /// Replaces the file with `remembered`, or removes it when nothing is left. The new text is
    /// written under a temporary name and renamed into place, so a reader finds the old file or
    /// the new one, whole. The directory is created on first use.
    pub(crate) fn store(&self, remembered: &Remembered) -> Result<(), Failure> {
        if remembered.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => {
                    Err(self.failed("removing", &err))
                }
                _ => Ok(()),
            };
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|err| self.failed("creating the directory of", &err))?;
        }
        let mut temporary = OsString::from(&self.path);
        temporary.push(format!(".{}.tmp", std::process::id()));
        let temporary = PathBuf::from(temporary);
        std::fs::write(&temporary, remembered.render())
            .and_then(|()| std::fs::rename(&temporary, &self.path))
            .map_err(|err| {
                drop(std::fs::remove_file(&temporary));
                self.failed("writing", &err)
            })
    }

    fn failed(&self, doing: &str, err: &io::Error) -> Failure {
        Failure::Other(format!("{doing} {}: {err}", self.path.display()))
    }
}

/// Stops `families` in one transaction of `session`, remembering the rate each had unless one is
/// remembered already, and gives what `file` remembers afterwards.
///
/// The rates are read and the file written while the transaction holds the write lock, so no other
/// controller changes a rate in between. A refused commit takes back the entries this run added.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the transaction is moved into `commit`, which releases its lock; the lint does not \
              see the move and suggests dropping it before the rates are read"
)]
pub(crate) fn freeze(
    session: &Controller,
    file: &StateFile,
    families: &[Family],
) -> Result<Remembered, Failure> {
    let transaction = session.begin()?;
    let info = session.info();
    let mut remembered = file.load()?;
    let added: Vec<Family> = families
        .iter()
        .copied()
        .filter(|&family| remembered.keep(family, info.family(family).rate.as_f64()))
        .collect();
    let steps: Vec<Step> = families
        .iter()
        .map(|&family| Step {
            family,
            change: Change::SetRate(0.0),
        })
        .collect();
    commit(transaction, &steps, file, &remembered, |now| {
        for &family in &added {
            now.forget(family);
        }
    })?;
    Ok(remembered)
}

/// Puts back the rate `file` remembers for each of `families` in one transaction of `session`,
/// and forgets them. A family with nothing remembered fails the whole run with nothing changed.
pub(crate) fn thaw(
    session: &Controller,
    file: &StateFile,
    families: &[Family],
) -> Result<(), Failure> {
    let transaction = session.begin()?;
    let mut remembered = file.load()?;
    let mut restored = Vec::new();
    let mut missing = Vec::new();
    for &family in families {
        match remembered.rate(family) {
            Some(rate) => restored.push((family, rate)),
            None => missing.push(family::label(family)),
        }
    }
    if !missing.is_empty() {
        return Err(Failure::Other(format!(
            "no remembered rate for {} of {}; set the rate outright with set-rate",
            missing.join(", "),
            session.path().display()
        )));
    }
    let steps: Vec<Step> = restored
        .iter()
        .map(|&(family, rate)| Step {
            family,
            change: Change::SetRate(rate),
        })
        .collect();
    for &(family, _) in &restored {
        remembered.forget(family);
    }
    commit(transaction, &steps, file, &remembered, |now| {
        for &(family, rate) in &restored {
            now.keep(family, rate);
        }
    })
}

/// Stages `steps`, writes `remembered` to `file`, and commits. When the commit is refused, `undo`
/// is applied to what the file holds by then.
fn commit(
    mut transaction: Transaction<'_>,
    steps: &[Step],
    file: &StateFile,
    remembered: &Remembered,
    undo: impl FnOnce(&mut Remembered),
) -> Result<(), Failure> {
    for step in steps {
        step.stage(&mut transaction)?;
    }
    file.store(remembered)?;
    transaction.commit().or_else(|refused| {
        let mut now = file.load()?;
        undo(&mut now);
        file.store(&now)?;
        Err(refused.into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            pairs
                .iter()
                .find(|&&(key, _)| key == name)
                .map(|&(_, value)| value.into())
        }
    }

    #[test]
    fn the_state_directory_falls_back_in_order() {
        let all = env(&[
            ("LIBTIMEWARP_STATE_DIR", "/named"),
            ("XDG_STATE_HOME", "/xdg"),
            ("HOME", "/home/u"),
        ]);
        assert_eq!(state_dir(&all), Some(PathBuf::from("/named")));
        let xdg = env(&[
            ("LIBTIMEWARP_STATE_DIR", ""),
            ("XDG_STATE_HOME", "/xdg"),
            ("HOME", "/home/u"),
        ]);
        assert_eq!(state_dir(&xdg), Some(PathBuf::from("/xdg/libtimewarp")));
        let home = env(&[("XDG_STATE_HOME", ""), ("HOME", "/home/u")]);
        assert_eq!(
            state_dir(&home),
            Some(PathBuf::from("/home/u/.local/state/libtimewarp"))
        );
        assert_eq!(state_dir(&env(&[])), None);
    }

    #[test]
    fn every_spelling_of_a_session_path_finds_one_file() {
        let dir = Path::new("/state");
        let at = |session: &str| {
            StateFile::under(dir, Path::new(session))
                .map(|file| file.path)
                .ok()
        };
        let expected = Some(PathBuf::from("/state/frozen/pid-1-x.session"));
        assert_eq!(at("./pid-1-x.session"), expected);
        assert_eq!(at("/tmp/libtimewarp/pid-1-x.session"), expected);
        assert_eq!(at("pid-1-x.session"), expected);
        assert!(at("/").is_none());
    }

    #[test]
    fn rates_read_back_to_the_same_value() {
        let mut remembered = Remembered::default();
        for (family, rate) in [
            (Family::Realtime, 0.1),
            (Family::Monotonic, 1.0 / 3.0),
            (Family::Boottime, 1e-9),
        ] {
            assert!(remembered.keep(family, rate));
        }
        let text = remembered.render();
        assert!(text.starts_with("realtime=0.1\nmonotonic="), "{text}");
        assert_eq!(Remembered::parse(&text), Ok(remembered));
    }

    #[test]
    fn the_first_remembered_rate_stays() {
        let mut remembered = Remembered::default();
        assert!(remembered.keep(Family::Realtime, 7.0));
        assert!(!remembered.keep(Family::Realtime, 0.0));
        assert_eq!(remembered.rate(Family::Realtime), Some(7.0));
        remembered.forget(Family::Realtime);
        assert!(remembered.is_empty());
    }

    #[test]
    fn a_line_that_is_not_an_entry_is_refused_by_number() {
        for text in [
            "realtime=fast\n",
            "realtime=1\nwallclock=1\n",
            "\nrealtime\n",
        ] {
            let refused = Remembered::parse(text);
            assert!(
                refused.as_ref().is_err_and(|why| why.starts_with("line ")),
                "{text:?}: {refused:?}"
            );
        }
        assert_eq!(
            Remembered::parse("\n Realtime = 2 \n").map(|r| r.rate(Family::Realtime)),
            Ok(Some(2.0))
        );
    }

    #[test]
    fn storing_writes_the_file_and_removes_it_when_empty() {
        let dir = tempfile::tempdir().expect("a directory");
        let file = StateFile::under(dir.path(), Path::new("s.session")).expect("a state file");
        assert_eq!(file.load().ok(), Some(Remembered::default()));

        let mut remembered = Remembered::default();
        remembered.keep(Family::Monotonic, 2.5);
        assert!(file.store(&remembered).is_ok());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("frozen/s.session")).ok(),
            Some("monotonic=2.5\n".to_owned())
        );
        assert_eq!(file.load().ok(), Some(remembered));

        assert!(file.store(&Remembered::default()).is_ok());
        assert!(!dir.path().join("frozen/s.session").exists());
        assert!(file.store(&Remembered::default()).is_ok());
        let left = std::fs::read_dir(dir.path().join("frozen"))
            .map(Iterator::count)
            .ok();
        assert_eq!(left, Some(0), "a temporary file was left behind");
    }
}
