//! What each command does to the session it picks.

use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use common::time::{Family, Nanos};
use controller::{Controller, Info};

use crate::args::{Command, Invocation, Picking};
use crate::failure::Failure;
use crate::output::Output;
use crate::{family, format};

/// Runs the command of `invocation`.
pub(crate) fn run<W: Write>(invocation: &Invocation, out: &mut Output<W>) -> Result<(), Failure> {
    match &invocation.command {
        Command::Info => info(&invocation.picking, out),
        Command::Get { family } => get(&invocation.picking, family, out),
    }
}

/// The session file the command acts on.
fn chosen(picking: &Picking) -> Result<PathBuf, Failure> {
    picking
        .session
        .clone()
        .ok_or_else(|| Failure::Other("no session named; pass --session PATH".to_owned()))
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
