//! What a session says about itself: how it was set up, what each family is doing, and what went
//! wrong with it.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use common::session::layout::{Fault, Header, Keeper, Params};
use common::sync;
use common::time::{Family, Nanos, Rate};

/// One family's settings, as the session holds them now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyInfo {
    /// Fake nanoseconds per nanosecond of the timebase.
    pub rate: Rate,
    /// The genuine clock passage is measured against, or `None` for Cpu, which scales each clock's
    /// own reading and so tracks no timebase at all.
    pub timebase: Option<libc::clockid_t>,
}

/// Everything a session reports about itself.
///
/// The three families that carry a line are read through the seqlock, as one whole copy, so the
/// rates and timebases here belong to one moment rather than to three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// Which timekeeper computes the families' values.
    pub keeper: Keeper,
    /// The hook library every process of the session loads, or `None` when it has none.
    pub hook: Option<PathBuf>,
    /// The process that created the session.
    pub creator_pid: u32,
    /// Genuine `CLOCK_REALTIME` when the session was created, which is what tells two files of one
    /// directory apart when their creators' pids have been recycled.
    pub creation_time: Nanos,
    /// Whether the session poses as the vDSO on Linux, or interposes the Go runtime's clock
    /// fallback routines on FreeBSD.
    pub emulate_vdso: bool,
    /// Whether a process that cannot set up what it was asked for exits rather than running
    /// genuine.
    pub fail_closed: bool,
    /// The Realtime family, the one a wall clock reads.
    pub realtime: FamilyInfo,
    /// The Monotonic family.
    pub monotonic: FamilyInfo,
    /// The Boottime family, which counts suspended time too.
    pub boottime: FamilyInfo,
    /// The Cpu family, whose rate is fixed at creation.
    pub cpu: FamilyInfo,
    /// Why the session stopped faking, or `None` while it still is.
    pub fault: Option<Fault>,
}

impl Info {
    /// Reads the header and one whole copy of the parameters.
    ///
    /// A null session comes through here as well as an affine one: its seqlock is unused, so the
    /// counter sits even and the read completes at once, over the genuine parameters its creator
    /// published.
    pub(crate) fn of(header: &Header) -> Self {
        let params = sync::read(header, Params::load);
        let tracking = |family: &common::session::layout::FamilyValue| FamilyInfo {
            rate: family.line.rate,
            timebase: Some(family.timebase),
        };
        Self {
            // `attach` refused a tag naming a keeper we do not know, so this one names a keeper.
            keeper: Keeper::from_tag(header.keeper).unwrap_or(Keeper::Null),
            hook: hook_path(header),
            creator_pid: header.creator_pid,
            creation_time: Nanos(header.creation_time),
            emulate_vdso: header.emulate_vdso != 0,
            fail_closed: header.fail_closed != 0,
            realtime: tracking(&params.realtime),
            monotonic: tracking(&params.monotonic),
            boottime: tracking(&params.boottime),
            cpu: FamilyInfo {
                rate: params.cpu_rate,
                timebase: None,
            },
            fault: header.fault(),
        }
    }

    /// One family's line of this report.
    #[must_use]
    pub const fn family(&self, family: Family) -> FamilyInfo {
        match family {
            Family::Realtime => self.realtime,
            Family::Monotonic => self.monotonic,
            Family::Boottime => self.boottime,
            Family::Cpu => self.cpu,
        }
    }
}

/// The hook library recorded in the header, or `None` when the field is empty, which is how a
/// session with no hook spells it.
fn hook_path(header: &Header) -> Option<PathBuf> {
    let bytes = header.hook_path();
    (!bytes.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(bytes)))
}
