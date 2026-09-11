//! One change to a session: the write lock from `begin`, operations staged in order, and one
//! write section at `commit`.

use std::io;
use std::path::Path;

use common::session::layout::{FamilyValue, Header, Keeper, ParamsValue};
use common::sync::{self, Anchor, WriteLock};
use common::time::{Family, Nanos, Rate};

use crate::Error;

/// What a move out of the range an instant can hold is refused with. Instants are signed 64-bit
/// nanoseconds, so the range spans about 1678 to 2262.
const OUT_OF_RANGE: &str = "the move would leave the range an instant can hold";

/// What an operation on Cpu is refused with.
const CPU_HAS_ONE_SETTING: &str =
    "Cpu has one setting, its rate, fixed at creation, and no single value it shows now";

/// What a rate the fixed point cannot carry is refused with.
const BAD_RATE: &str = "a rate must be a number at or above zero and below the ceiling, 2^32";

/// Which of the three lines in a copy of the parameters an operation lands on.
///
/// Cpu carries no line, and is refused where an operation is staged, so every staged operation
/// names one of these.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Realtime,
    Monotonic,
    Boottime,
}

impl Slot {
    /// The slot a family's operations land in, or `None` for Cpu.
    const fn of(family: Family) -> Option<Self> {
        match family {
            Family::Realtime => Some(Self::Realtime),
            Family::Monotonic => Some(Self::Monotonic),
            Family::Boottime => Some(Self::Boottime),
            Family::Cpu => None,
        }
    }

    /// The family this slot holds, for the error that names it.
    const fn family(self) -> Family {
        match self {
            Self::Realtime => Family::Realtime,
            Self::Monotonic => Family::Monotonic,
            Self::Boottime => Family::Boottime,
        }
    }

    /// This slot's line inside a working copy of the parameters.
    const fn within(self, params: &mut ParamsValue) -> &mut FamilyValue {
        match self {
            Self::Realtime => &mut params.realtime,
            Self::Monotonic => &mut params.monotonic,
            Self::Boottime => &mut params.boottime,
        }
    }
}

/// One staged change. Staging writes nothing to the session; `commit` is what applies these.
#[derive(Debug, Clone, Copy)]
enum Operation {
    /// Move the family by a duration.
    Shift { slot: Slot, by: Nanos },
    /// Make the family show a value now.
    SetTime { slot: Slot, to: Nanos },
    /// Change how fast the family runs.
    SetRate { slot: Slot, rate: Rate },
}

impl Operation {
    const fn slot(self) -> Slot {
        match self {
            Self::Shift { slot, .. } | Self::SetTime { slot, .. } | Self::SetRate { slot, .. } => {
                slot
            }
        }
    }

    /// Whether this operation moves the family's value rather than its speed.
    const fn is_a_step(self) -> bool {
        matches!(self, Self::Shift { .. } | Self::SetTime { .. })
    }

    /// **W6** for one operation: the family re-anchored at the **W5** instant, showing what this
    /// operation asked for.
    ///
    /// `params` is the working copy the operations before this one already landed on, so applying
    /// the staged operations in order is applying each to what the one before it left.
    fn apply(self, params: &mut ParamsValue, anchor: &Anchor) -> Result<(), Error> {
        let slot = self.slot();
        let line = slot.within(params);
        let Some(now) = anchor.of(line.timebase) else {
            return Err(Error::Io(io::Error::other(
                "the family's timebase would not read inside the write section",
            )));
        };
        // What the family shows at the anchor: the value that bounds everything readers have
        // received, because no read completes between the counter going odd and this instant.
        let showing = line.line.project(now);

        let requested = match self {
            Self::Shift { by, .. } => {
                // Saturating for the comparison and checked for the value: a backwards move is
                // named as backwards whatever its magnitude, and a forward one that would leave
                // the range an instant holds is refused rather than wrapped.
                refuse_backwards(slot, showing, showing.saturating_add(by))?;
                Nanos(
                    showing
                        .0
                        .checked_add(by.0)
                        .ok_or(Error::BadArgument(OUT_OF_RANGE))?,
                )
            }
            Self::SetTime { to, .. } => {
                refuse_backwards(slot, showing, to)?;
                to
            }
            Self::SetRate { rate, .. } => {
                line.line.rate = rate;
                showing
            }
        };
        // Both anchors move together, so the old and new lines agree at the anchor and the time
        // readers observe is continuous across everything but the step the user asked for.
        line.line.fake_anchor = requested;
        line.line.timebase_anchor = now;

        if self.is_a_step() {
            // A move of the value is the user's own discontinuity, which the FreeBSD republisher
            // follows rather than smooths away; a change of speed leaves the line where it is.
            params.epoch = params.epoch.wrapping_add(1);
        }
        Ok(())
    }
}

/// Monotonic and Boottime refuse a value below what the family shows at the anchor.
///
/// The comparison sits here, inside the section, rather than before it: the anchor is the one
/// value that bounds everything readers have already received, and a value compared before the
/// section was already stale when the comparison finished.
fn refuse_backwards(slot: Slot, showing: Nanos, requested: Nanos) -> Result<(), Error> {
    let family = slot.family();
    if family.is_nondecreasing() && requested < showing {
        return Err(Error::WouldGoBackwards(family));
    }
    Ok(())
}

/// A change in flight: the write lock is held and nothing is written until [`Transaction::commit`].
///
/// The sequence counter stays even from `begin` to `commit`, so every reader in the session keeps
/// reading the old parameters and only other writers wait. A controller that dies in between
/// releases the lock through the kernel and leaves nothing behind, because **W2** never ran.
#[derive(Debug)]
pub struct Transaction<'a> {
    header: &'a Header,
    lock: WriteLock,
    staged: Vec<Operation>,
}

impl<'a> Transaction<'a> {
    /// **W1** over a session the caller already holds mapped: takes the write lock, and nothing
    /// else.
    ///
    /// The sequence counter stays even, so every reader in the session keeps reading the old
    /// parameters until the commit and only other writers wait. The wait has no bound: a held lock
    /// means a live writer that will finish.
    ///
    /// [`Controller::begin`](crate::Controller::begin) is this over the session a controller
    /// attached. A target that sets the time is a writer for that one call and comes through here
    /// with the mapping its own constructor made, so a setter opens no second view of the session.
    ///
    /// # Errors
    /// [`Error::UnsupportedForKeeper`] on a null session, which has no parameters to change, and
    /// [`Error::Io`] when the lock file cannot be opened, is not ours, or cannot be locked.
    pub fn begin(header: &'a Header, lock_path: &Path) -> Result<Self, Error> {
        if Keeper::from_tag(header.keeper) != Some(Keeper::Affine) {
            return Err(Error::UnsupportedForKeeper);
        }
        Ok(Self::new(header, sync::begin(header, lock_path)?))
    }

    /// A transaction over `header`, holding the write lock `lock`, with nothing staged yet.
    const fn new(header: &'a Header, lock: WriteLock) -> Self {
        Self {
            header,
            lock,
            staged: Vec::new(),
        }
    }

    /// Stages a move of `family` by a duration. A change lands on every clock of the family.
    ///
    /// Nothing is written to the session, and a refusal here leaves the transaction as it was, so
    /// the caller may stage something else or abort.
    ///
    /// # Errors
    /// [`Error::BadArgument`] for Cpu, which has one setting and no single value it shows now.
    pub fn shift(&mut self, family: Family, by: Nanos) -> Result<(), Error> {
        let slot = Slot::of(family).ok_or(Error::BadArgument(CPU_HAS_ONE_SETTING))?;
        self.staged.push(Operation::Shift { slot, by });
        Ok(())
    }

    /// Stages a value for `family` to show now. A change lands on every clock of the family.
    ///
    /// Whether the value is one the family may take is decided at [`Transaction::commit`], against
    /// the anchor sampled inside the write section.
    ///
    /// # Errors
    /// [`Error::BadArgument`] for Cpu, which has one setting and no single value it shows now.
    pub fn set_time(&mut self, family: Family, to: Nanos) -> Result<(), Error> {
        let slot = Slot::of(family).ok_or(Error::BadArgument(CPU_HAS_ONE_SETTING))?;
        self.staged.push(Operation::SetTime { slot, to });
        Ok(())
    }

    /// Stages how fast `family` runs: fake nanoseconds per nanosecond of its timebase. A rate of
    /// zero freezes the family.
    ///
    /// A rate crosses this edge as a `double` and converts to the library's 32.32 fixed point
    /// here, which is the one place in the library a floating point rate appears.
    ///
    /// # Errors
    /// [`Error::BadArgument`] for a negative rate, a rate at or above the ceiling, a rate that is
    /// not a number, or for Cpu, whose rate is fixed at creation because a writer in another
    /// process cannot read the target thread's own CPU clock.
    pub fn set_rate(&mut self, family: Family, rate: f64) -> Result<(), Error> {
        let slot = Slot::of(family).ok_or(Error::BadArgument(CPU_HAS_ONE_SETTING))?;
        let rate = Rate::from_f64(rate).ok_or(Error::BadArgument(BAD_RATE))?;
        self.staged.push(Operation::SetRate { slot, rate });
        Ok(())
    }

    /// **W2** through **W12**: the staged operations applied in order against one anchor, and
    /// published once.
    ///
    /// The whole transaction is one write section, so a reader sees all of it or none of it, and
    /// three families change together against timebases sampled inside that one section.
    ///
    /// # Errors
    /// Whatever the first refused operation refused with. A refusal aborts the whole transaction
    /// with nothing written: **W6** through **W9** never ran and the shadow-complete flag is still
    /// clear, so the section closes looking to a reader exactly like a writer that died between
    /// **W3** and **W7**.
    pub fn commit(self) -> Result<(), Error> {
        let Self {
            header,
            lock,
            staged,
        } = self;
        let published = lock.publish(header, |anchor, live| {
            let mut next = live;
            for operation in staged {
                operation.apply(&mut next, anchor)?;
            }
            Ok(next)
        });
        drop(lock); // W12
        published
    }

    /// Discards the staged operations and releases the write lock, having written nothing.
    pub fn abort(self) {
        drop(self);
    }
}
