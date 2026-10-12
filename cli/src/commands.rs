//! What each command does to the session it picks.

use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use common::time::{Family, Nanos, Rate};
use controller::{Controller, Info};

use crate::args::{Command, Invocation, Operation, Picking};
use crate::change::{self, Step};
use crate::failure::Failure;
use crate::frozen::StateFile;
use crate::output::Output;
use crate::{directory, environ, family, format, frozen};

/// Runs the command of `invocation`.
pub(crate) fn run<W: Write>(invocation: &Invocation, out: &mut Output<W>) -> Result<(), Failure> {
    match &invocation.command {
        Command::List => list(&invocation.picking, out),
        Command::Info => info(&invocation.picking, out),
        Command::Get { family } => get(&invocation.picking, family, out),
        Command::Change(operations) => change(&invocation.picking, operations, out),
        Command::Freeze { families } => freeze(&invocation.picking, families, out),
        Command::Thaw { families } => thaw(&invocation.picking, families, out),
    }
}

/// The directory sessions are looked for in: `--dir`, else the one the library computes.
fn directory(picking: &Picking) -> Result<PathBuf, Failure> {
    let dir = match &picking.dir {
        Some(dir) => dir.clone(),
        None => directory::session_dir()?,
    };
    Ok(directory::tidy(&dir))
}

/// The session file the command acts on: `--session`, else the one `--pid` carries, else the pick
/// out of the directory.
fn chosen(picking: &Picking) -> Result<PathBuf, Failure> {
    if let Some(session) = &picking.session {
        return Ok(directory::tidy(session));
    }
    if let Some(pid) = picking.pid {
        return environ::shared_file_of(pid).map(|path| directory::tidy(&path));
    }
    directory::select(&directory(picking)?, picking.creator, picking.newest).map(|found| found.path)
}

fn list<W: Write>(picking: &Picking, out: &mut Output<W>) -> Result<(), Failure> {
    let dir = directory(picking)?;
    out.say_bytes("directory", dir.as_os_str().as_bytes())?;
    let found = directory::sessions(&dir);
    for (index, one) in found.iter().enumerate() {
        let key = |name: &str| format!("session.{index}.{name}");
        out.say_bytes(&key("path"), one.path.as_os_str().as_bytes())?;
        out.say(&key("creator-pid"), one.info.creator_pid)?;
        out.say(&key("created"), format::instant(one.info.creation_time))?;
        out.say(&key("created-ns"), one.info.creation_time.0)?;
        out.say(&key("keeper"), format::keeper(one.info.keeper))?;
        out.say(&key("fault"), format::fault(one.info.fault))?;
    }
    out.say("count", found.len())
}

fn info<W: Write>(picking: &Picking, out: &mut Output<W>) -> Result<(), Failure> {
    let path = chosen(picking)?;
    let session = Controller::attach(&path)?;
    report(out, &path, &session.info())
}

fn get<W: Write>(picking: &Picking, name: &str, out: &mut Output<W>) -> Result<(), Failure> {
    let family = family::named(name)?;
    let session = Controller::attach(&chosen(picking)?)?;
    reading(out, family, session.get(family)?)
}

/// Parses every operation, then attaches, applies them as one transaction, and prints each family
/// they touched.
fn change<W: Write>(
    picking: &Picking,
    operations: &[Operation],
    out: &mut Output<W>,
) -> Result<(), Failure> {
    let steps = operations
        .iter()
        .map(Step::parse)
        .collect::<Result<Vec<_>, _>>()?;
    let session = Controller::attach(&chosen(picking)?)?;
    change::apply(&session, &steps)?;
    for touched in change::touched(&steps) {
        if touched.reading {
            reading(out, touched.family, session.get(touched.family)?)?;
        }
        if touched.rate {
            let rate = session.info().family(touched.family).rate;
            out.say(
                &format!("{}.rate", family::label(touched.family)),
                format::rate(rate),
            )?;
        }
    }
    Ok(())
}

/// The families `names` spell, each once, in order; Realtime, Monotonic, and Boottime when there
/// are none.
fn families(names: &[String]) -> Result<Vec<Family>, Failure> {
    if names.is_empty() {
        return Ok(vec![Family::Realtime, Family::Monotonic, Family::Boottime]);
    }
    let mut families = Vec::new();
    for name in names {
        let family = family::named(name)?;
        if !families.contains(&family) {
            families.push(family);
        }
    }
    Ok(families)
}

/// Adds each family's rate to the remembered rates and stops it, in one transaction.
fn freeze<W: Write>(
    picking: &Picking,
    names: &[String],
    out: &mut Output<W>,
) -> Result<(), Failure> {
    let families = families(names)?;
    let path = chosen(picking)?;
    let file = StateFile::of(&path)?;
    let session = Controller::attach(&path)?;
    let remembered = frozen::freeze(&session, &file, &families)?;
    for family in families {
        let label = family::label(family);
        out.say(&format!("{label}.rate"), format::rate(Rate::FROZEN))?;
        let rate = remembered.rate(family).unwrap_or_default();
        out.say(&format!("{label}.remembered"), format::fraction(rate))?;
    }
    Ok(())
}

/// Puts back the remembered rates, in one transaction, and prints them as read back.
fn thaw<W: Write>(picking: &Picking, names: &[String], out: &mut Output<W>) -> Result<(), Failure> {
    let families = families(names)?;
    let path = chosen(picking)?;
    let file = StateFile::of(&path)?;
    let session = Controller::attach(&path)?;
    frozen::thaw(&session, &path, &file, &families)?;
    let info = session.info();
    for family in families {
        out.say(
            &format!("{}.rate", family::label(family)),
            format::rate(info.family(family).rate),
        )?;
    }
    Ok(())
}

/// What `family` shows: `<family>.ns`, then `realtime.iso` or `<family>.elapsed`.
pub(crate) fn reading<W: Write>(
    out: &mut Output<W>,
    family: Family,
    nanos: Nanos,
) -> Result<(), Failure> {
    let label = family::label(family);
    out.say(&format!("{label}.ns"), nanos.0)?;
    if family == Family::Realtime {
        out.say(&format!("{label}.iso"), format::instant(nanos))
    } else {
        out.say(&format!("{label}.elapsed"), format::elapsed(nanos))
    }
}

/// Everything `info` prints about the session at `path`.
fn report<W: Write>(out: &mut Output<W>, path: &Path, info: &Info) -> Result<(), Failure> {
    out.say_bytes("session", path.as_os_str().as_bytes())?;
    out.say("keeper", format::keeper(info.keeper))?;
    out.say_bytes(
        "hook",
        info.hook
            .as_deref()
            .map_or(&[][..], |hook| hook.as_os_str().as_bytes()),
    )?;
    out.say("creator-pid", info.creator_pid)?;
    out.say("created", format::instant(info.creation_time))?;
    out.say("created-ns", info.creation_time.0)?;
    out.say("emulate-vdso", u8::from(info.emulate_vdso))?;
    out.say("fail-closed", u8::from(info.fail_closed))?;
    out.say("fault", format::fault(info.fault))?;
    out.say("fault-pid", format::fault_pid(info.fault))?;
    for family in Family::ALL {
        let label = family::label(family);
        let line = info.family(family);
        out.say(&format!("{label}.rate"), format::rate(line.rate))?;
        out.say(
            &format!("{label}.timebase"),
            format::timebase(line.timebase),
        )?;
    }
    Ok(())
}
