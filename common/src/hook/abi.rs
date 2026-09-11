//! The shapes a hook library sees: the call struct, the function enumeration, the four kinds, and
//! the two status values.
//!
//! `include/libtimewarp_hook.h` is the same definitions in C, and it is the one a hook author
//! reads. The test at the bottom checks the two agree name for name and number for number, so a
//! function added on this side without the other is a failure rather than a silent mismatch.

use std::ffi::c_void;

use crate::platform;

/// The fixed symbol every interposed call is channelled to.
pub const ANSWER_SYMBOL: &[u8] = b"libtimewarp_hook\0";

/// The symbol that runs once, at session creation. Optional: a hook that fills nothing skips it.
pub const INIT_SYMBOL: &[u8] = b"libtimewarp_hook_init\0";

/// What a hook says about a call.
///
/// Status is a separate return and never a sentinel, because every `i64` is a legal answer.
/// Declining is zero, so a hook whose switch falls through leaves the keeper's answer standing
/// rather than handing the program whatever `*out` happened to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Status {
    /// The keeper answers this call.
    Declined = 0,
    /// `*out` is what the program sees, unaltered.
    Handled = 1,
}

impl Status {
    /// The status a hook's return value names. Anything we do not know declines, because a number
    /// we cannot read says nothing about `*out`.
    #[must_use]
    pub const fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Handled,
            _ => Self::Declined,
        }
    }

    /// The name `libtimewarp_hook.h` spells this status with.
    #[must_use]
    pub const fn c_name(self) -> &'static str {
        match self {
            Self::Declined => "LIBTIMEWARP_DECLINED",
            Self::Handled => "LIBTIMEWARP_HANDLED",
        }
    }

    /// Both statuses, so a test can walk them.
    pub const ALL: [Self; 2] = [Self::Declined, Self::Handled];
}

/// Which of the four conversions a call is, so a hook can ignore [`Func`] and switch on the shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Kind {
    /// A genuine reading becomes the fake one: clock reads, timestamp reads.
    InstantToFake = 1,
    /// A fake instant becomes the genuine one: timestamp sets, clock sets.
    InstantToGenuine = 2,
    /// A fake duration becomes the genuine one: sleeps, timeouts, timer arms.
    DurationToGenuine = 3,
    /// A genuine remaining becomes the fake one: timer queries.
    DurationToFake = 4,
}

impl Kind {
    /// Every kind, in the order the conversion table lists them.
    pub const ALL: [Self; 4] = [
        Self::InstantToFake,
        Self::InstantToGenuine,
        Self::DurationToGenuine,
        Self::DurationToFake,
    ];

    /// The name `libtimewarp_hook.h` spells this kind with.
    #[must_use]
    pub const fn c_name(self) -> &'static str {
        match self {
            Self::InstantToFake => "LIBTIMEWARP_INSTANT_TO_FAKE",
            Self::InstantToGenuine => "LIBTIMEWARP_INSTANT_TO_GENUINE",
            Self::DurationToGenuine => "LIBTIMEWARP_DURATION_TO_GENUINE",
            Self::DurationToFake => "LIBTIMEWARP_DURATION_TO_FAKE",
        }
    }
}

/// Declares [`Func`] and the table pairing each variant with the name the C header spells it with.
macro_rules! funcs {
    ($( $(#[$doc:meta])* $variant:ident = $value:literal, $c_name:literal; )*) => {
        /// Which interposed function a call answers.
        ///
        /// The enumeration covers every function the library takes over on any platform, so a hook
        /// built once runs against all of them. A value is fixed for good: a new function takes a
        /// new number rather than renumbering its neighbours. Aliases report the name they alias,
        /// so glibc's `__clock_gettime` and `syscall(SYS_clock_gettime)` both arrive as
        /// [`Func::ClockGettime`].
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u32)]
        pub enum Func {
            $( $(#[$doc])* $variant = $value, )*
        }

        impl Func {
            /// Every function, each with the name `libtimewarp_hook.h` spells it with.
            pub const ALL: &'static [(Self, &'static str)] = &[
                $( (Self::$variant, $c_name), )*
            ];
        }
    };
}

funcs! {
    /// `clock_gettime`, and the aliases and syscall numbers that reach it.
    ClockGettime = 1, "LIBTIMEWARP_FUNC_CLOCK_GETTIME";
    /// `gettimeofday`.
    Gettimeofday = 2, "LIBTIMEWARP_FUNC_GETTIMEOFDAY";
    /// `time`, whose answer is the nanoseconds truncated to whole seconds.
    Time = 3, "LIBTIMEWARP_FUNC_TIME";
    /// `timespec_get`.
    TimespecGet = 4, "LIBTIMEWARP_FUNC_TIMESPEC_GET";
    /// `ftime`.
    Ftime = 5, "LIBTIMEWARP_FUNC_FTIME";
    /// `clock`, the process's consumed CPU time.
    Clock = 6, "LIBTIMEWARP_FUNC_CLOCK";
    /// `times`.
    Times = 7, "LIBTIMEWARP_FUNC_TIMES";
    /// `getrusage`.
    Getrusage = 8, "LIBTIMEWARP_FUNC_GETRUSAGE";
    /// macOS `mach_absolute_time`.
    MachAbsoluteTime = 9, "LIBTIMEWARP_FUNC_MACH_ABSOLUTE_TIME";
    /// macOS `mach_continuous_time`.
    MachContinuousTime = 10, "LIBTIMEWARP_FUNC_MACH_CONTINUOUS_TIME";
    /// macOS `mach_approximate_time`.
    MachApproximateTime = 11, "LIBTIMEWARP_FUNC_MACH_APPROXIMATE_TIME";
    /// macOS `mach_continuous_approximate_time`.
    MachContinuousApproximateTime = 12, "LIBTIMEWARP_FUNC_MACH_CONTINUOUS_APPROXIMATE_TIME";
    /// macOS `clock_gettime_nsec_np`.
    ClockGettimeNsecNp = 13, "LIBTIMEWARP_FUNC_CLOCK_GETTIME_NSEC_NP";
    /// macOS `clock_get_time` on a clock service.
    ClockGetTime = 14, "LIBTIMEWARP_FUNC_CLOCK_GET_TIME";

    /// `nanosleep`.
    Nanosleep = 100, "LIBTIMEWARP_FUNC_NANOSLEEP";
    /// `sleep`.
    Sleep = 101, "LIBTIMEWARP_FUNC_SLEEP";
    /// `usleep`.
    Usleep = 102, "LIBTIMEWARP_FUNC_USLEEP";
    /// `thrd_sleep`.
    ThrdSleep = 103, "LIBTIMEWARP_FUNC_THRD_SLEEP";
    /// `clock_nanosleep`.
    ClockNanosleep = 104, "LIBTIMEWARP_FUNC_CLOCK_NANOSLEEP";
    /// macOS `mach_wait_until`.
    MachWaitUntil = 105, "LIBTIMEWARP_FUNC_MACH_WAIT_UNTIL";

    /// `poll`.
    Poll = 200, "LIBTIMEWARP_FUNC_POLL";
    /// `ppoll`.
    Ppoll = 201, "LIBTIMEWARP_FUNC_PPOLL";
    /// `select`.
    Select = 202, "LIBTIMEWARP_FUNC_SELECT";
    /// `pselect`.
    Pselect = 203, "LIBTIMEWARP_FUNC_PSELECT";
    /// `epoll_wait`.
    EpollWait = 204, "LIBTIMEWARP_FUNC_EPOLL_WAIT";
    /// `epoll_pwait`.
    EpollPwait = 205, "LIBTIMEWARP_FUNC_EPOLL_PWAIT";
    /// `epoll_pwait2`.
    EpollPwait2 = 206, "LIBTIMEWARP_FUNC_EPOLL_PWAIT2";
    /// `kevent`.
    Kevent = 207, "LIBTIMEWARP_FUNC_KEVENT";
    /// `pthread_cond_timedwait`.
    PthreadCondTimedwait = 208, "LIBTIMEWARP_FUNC_PTHREAD_COND_TIMEDWAIT";
    /// `pthread_cond_clockwait`.
    PthreadCondClockwait = 209, "LIBTIMEWARP_FUNC_PTHREAD_COND_CLOCKWAIT";
    /// `pthread_mutex_timedlock`.
    PthreadMutexTimedlock = 210, "LIBTIMEWARP_FUNC_PTHREAD_MUTEX_TIMEDLOCK";
    /// `pthread_rwlock_timedrdlock`.
    PthreadRwlockTimedrdlock = 211, "LIBTIMEWARP_FUNC_PTHREAD_RWLOCK_TIMEDRDLOCK";
    /// `pthread_rwlock_timedwrlock`.
    PthreadRwlockTimedwrlock = 212, "LIBTIMEWARP_FUNC_PTHREAD_RWLOCK_TIMEDWRLOCK";
    /// `pthread_timedjoin_np`.
    PthreadTimedjoinNp = 213, "LIBTIMEWARP_FUNC_PTHREAD_TIMEDJOIN_NP";
    /// `sem_timedwait`.
    SemTimedwait = 214, "LIBTIMEWARP_FUNC_SEM_TIMEDWAIT";
    /// `sem_clockwait`.
    SemClockwait = 215, "LIBTIMEWARP_FUNC_SEM_CLOCKWAIT";
    /// `sigtimedwait`.
    Sigtimedwait = 216, "LIBTIMEWARP_FUNC_SIGTIMEDWAIT";
    /// `aio_suspend`.
    AioSuspend = 217, "LIBTIMEWARP_FUNC_AIO_SUSPEND";
    /// `recvmmsg`.
    Recvmmsg = 218, "LIBTIMEWARP_FUNC_RECVMMSG";
    /// `cnd_timedwait`.
    CndTimedwait = 219, "LIBTIMEWARP_FUNC_CND_TIMEDWAIT";
    /// `mtx_timedlock`.
    MtxTimedlock = 220, "LIBTIMEWARP_FUNC_MTX_TIMEDLOCK";
    /// `mq_timedsend`.
    MqTimedsend = 221, "LIBTIMEWARP_FUNC_MQ_TIMEDSEND";
    /// `mq_timedreceive`.
    MqTimedreceive = 222, "LIBTIMEWARP_FUNC_MQ_TIMEDRECEIVE";

    /// `setitimer`.
    Setitimer = 300, "LIBTIMEWARP_FUNC_SETITIMER";
    /// `getitimer`.
    Getitimer = 301, "LIBTIMEWARP_FUNC_GETITIMER";
    /// `alarm`.
    Alarm = 302, "LIBTIMEWARP_FUNC_ALARM";
    /// `timer_settime`.
    TimerSettime = 303, "LIBTIMEWARP_FUNC_TIMER_SETTIME";
    /// `timer_gettime`.
    TimerGettime = 304, "LIBTIMEWARP_FUNC_TIMER_GETTIME";
    /// `timerfd_settime`.
    TimerfdSettime = 305, "LIBTIMEWARP_FUNC_TIMERFD_SETTIME";
    /// `timerfd_gettime`.
    TimerfdGettime = 306, "LIBTIMEWARP_FUNC_TIMERFD_GETTIME";

    /// `stat`, and the `64` and `$INODE64` variants of it.
    Stat = 400, "LIBTIMEWARP_FUNC_STAT";
    /// `fstat`.
    Fstat = 401, "LIBTIMEWARP_FUNC_FSTAT";
    /// `lstat`.
    Lstat = 402, "LIBTIMEWARP_FUNC_LSTAT";
    /// `fstatat`.
    Fstatat = 403, "LIBTIMEWARP_FUNC_FSTATAT";
    /// Linux `statx`.
    Statx = 404, "LIBTIMEWARP_FUNC_STATX";
    /// `utime`.
    Utime = 405, "LIBTIMEWARP_FUNC_UTIME";
    /// `utimes`.
    Utimes = 406, "LIBTIMEWARP_FUNC_UTIMES";
    /// `futimes`.
    Futimes = 407, "LIBTIMEWARP_FUNC_FUTIMES";
    /// `lutimes`.
    Lutimes = 408, "LIBTIMEWARP_FUNC_LUTIMES";
    /// `utimensat`.
    Utimensat = 409, "LIBTIMEWARP_FUNC_UTIMENSAT";
    /// `futimens`.
    Futimens = 410, "LIBTIMEWARP_FUNC_FUTIMENS";

    /// `settimeofday`.
    Settimeofday = 500, "LIBTIMEWARP_FUNC_SETTIMEOFDAY";
    /// `clock_settime`.
    ClockSettime = 501, "LIBTIMEWARP_FUNC_CLOCK_SETTIME";
    /// `adjtime`.
    Adjtime = 502, "LIBTIMEWARP_FUNC_ADJTIME";
}

/// One interposed call, as a hook sees it.
///
/// It travels by pointer, so a field added later is no break: a hook reads the fields it knows and
/// ignores the rest. Every number is nanoseconds, whatever units the function itself uses.
#[derive(Debug)]
#[repr(C)]
pub struct Call {
    /// Which interposed function this is, a [`Func`].
    pub func: u32,
    /// Which of the four conversions, a [`Kind`].
    pub kind: u32,
    /// The clock id the call names, or `CLOCK_REALTIME` when it names none.
    pub clock: i32,
    /// The number to convert.
    pub value: i64,
    /// The genuine reading of `clock` at the point of interposition.
    pub real_ns: i64,
    /// What the keeper would return for this call. Declining returns exactly this.
    pub keeper: i64,
    /// The shared user state, or null when `size` is 0.
    pub arena: *mut c_void,
    /// The arena's size in bytes, fixed at session creation.
    pub size: u64,
}

impl Call {
    /// The call a hook is handed, with no arena. [`Call::with_arena`] puts one in.
    #[must_use]
    pub fn new(
        func: Func,
        kind: Kind,
        clock: libc::clockid_t,
        value: i64,
        real_ns: i64,
        keeper: i64,
    ) -> Self {
        Self {
            func: func as u32,
            kind: kind as u32,
            clock: platform::clock_id(clock),
            value,
            real_ns,
            keeper,
            arena: std::ptr::null_mut(),
            size: 0,
        }
    }

    /// The same call, carrying the shared state a hook reads and writes.
    #[must_use]
    pub const fn with_arena(mut self, arena: *mut c_void, size: u64) -> Self {
        self.arena = arena;
        self.size = size;
        self
    }
}

const _: () = {
    assert!(size_of::<Call>() == 56);
    assert!(align_of::<Call>() == 8);
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The C header, which is the definition a hook author reads.
    const HEADER: &str = include_str!("../../../include/libtimewarp_hook.h");

    /// The value the header gives an enumerator, or `None` when it names none.
    fn enumerator(name: &str) -> Option<i64> {
        HEADER
            .lines()
            .map(|line| line.trim().trim_end_matches(','))
            .filter_map(|line| line.split_once(" = "))
            .find(|(spelling, _)| *spelling == name)
            .and_then(|(_, value)| value.trim().parse().ok())
    }

    #[test]
    fn the_call_struct_is_the_documented_shape() {
        use std::mem::offset_of;
        assert_eq!(offset_of!(Call, func), 0);
        assert_eq!(offset_of!(Call, kind), 4);
        assert_eq!(offset_of!(Call, clock), 8);
        assert_eq!(offset_of!(Call, value), 16);
        assert_eq!(offset_of!(Call, real_ns), 24);
        assert_eq!(offset_of!(Call, keeper), 32);
        assert_eq!(offset_of!(Call, arena), 40);
        assert_eq!(offset_of!(Call, size), 48);
        assert_eq!(size_of::<Call>(), 56);
    }

    #[test]
    fn every_function_carries_the_number_the_shipped_header_gives_it() {
        for &(func, c_name) in Func::ALL {
            assert_eq!(
                enumerator(c_name),
                Some(i64::from(func as u32)),
                "{c_name} disagrees with the shipped header"
            );
        }
    }

    #[test]
    fn the_shipped_header_names_no_function_this_side_is_missing() {
        let ours: Vec<&str> = Func::ALL.iter().map(|&(_, name)| name).collect();
        for line in HEADER.lines().map(str::trim) {
            let Some(name) = line.split(' ').next().filter(|name| {
                name.starts_with("LIBTIMEWARP_FUNC_") && *name != "LIBTIMEWARP_FUNC_NONE"
            }) else {
                continue;
            };
            assert!(ours.contains(&name), "{name} is in the header alone");
        }
    }

    #[test]
    fn the_kinds_and_the_statuses_agree_with_the_shipped_header() {
        for kind in Kind::ALL {
            assert_eq!(enumerator(kind.c_name()), Some(i64::from(kind as u32)));
        }
        for status in Status::ALL {
            assert_eq!(enumerator(status.c_name()), Some(i64::from(status as i32)));
        }
        // Zero names no function and no kind, so a zeroed call asks for nothing.
        assert_eq!(enumerator("LIBTIMEWARP_FUNC_NONE"), Some(0));
        assert_eq!(enumerator("LIBTIMEWARP_KIND_NONE"), Some(0));
    }

    #[test]
    fn no_two_functions_share_a_number_or_a_name() {
        let mut numbers: Vec<u32> = Func::ALL.iter().map(|&(func, _)| func as u32).collect();
        numbers.sort_unstable();
        let count = numbers.len();
        numbers.dedup();
        assert_eq!(numbers.len(), count, "two functions share a number");

        let mut names: Vec<&str> = Func::ALL.iter().map(|&(_, name)| name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "two functions share a name");
    }

    #[test]
    fn a_status_we_do_not_know_declines() {
        assert_eq!(Status::from_code(0), Status::Declined);
        assert_eq!(Status::from_code(1), Status::Handled);
        for code in [-1, 2, i32::MIN, i32::MAX] {
            assert_eq!(Status::from_code(code), Status::Declined, "{code}");
        }
    }

    #[test]
    fn a_call_carries_what_it_was_built_from() {
        let call = Call::new(
            Func::Nanosleep,
            Kind::DurationToGenuine,
            libc::CLOCK_MONOTONIC,
            7,
            11,
            13,
        );
        assert_eq!(call.func, Func::Nanosleep as u32);
        assert_eq!(call.kind, Kind::DurationToGenuine as u32);
        assert_eq!(call.clock, libc::CLOCK_MONOTONIC);
        assert_eq!((call.value, call.real_ns, call.keeper), (7, 11, 13));
        assert!(call.arena.is_null(), "a call with no arena carries none");
        assert_eq!(call.size, 0);

        let mut word = 0u64;
        let arena = (&raw mut word).cast::<c_void>();
        let shared = call.with_arena(arena, 8);
        assert_eq!(shared.arena, arena);
        assert_eq!(shared.size, 8);
    }
}
