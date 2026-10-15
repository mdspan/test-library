//! Which clock a POSIX timer or a timerfd was created on.
//!
//! `timer_settime` and `timerfd_settime` name no clock: a timer arms against whichever clock it
//! was created on. So the clock of a POSIX timer is learned where it is chosen, at `timer_create`,
//! looked up again at the arm and the query, and forgotten at `timer_delete`. Every clock is kept,
//! the default included, because nothing makes a timer without asking for one, so a timer with
//! nothing kept is one whose clock is unknown rather than one on the default. Neither call reads
//! the time.
//!
//! A timerfd's clock is the kernel's to say, in the description `/proc` keeps of every descriptor,
//! because a descriptor reaches a program by more ways than `timerfd_create`: `dup`, `dup2`,
//! `dup3` and `fcntl` hand out other numbers for it, `close` lets its number go to something else,
//! and an exec or a socket brings one in from elsewhere. The clock `timerfd_create` named is kept
//! too, by number, for a process that cannot read `/proc`, and nothing forgets it.
//!
//! Two tables, because a descriptor and a timer id are numbers of different kinds and either could
//! collide with the other. [`crate::clocks`] holds the slots. The timerfd is Linux's alone, so its
//! table is there only where the calls are.

use crate::clocks::Table;

/// Every POSIX timer this process remembers, by the id `timer_create` handed back.
static TIMERS: Table = Table::new();

/// Every timerfd this process remembers, by its descriptor.
#[cfg(target_os = "linux")]
static TIMERFDS: Table = Table::new();

/// The key a timer id is. glibc counts these up from zero, so the number itself is the key.
fn timer_key(timer: libc::timer_t) -> usize {
    timer.addr()
}

/// The key a descriptor is, or `None` for one no `timerfd_create` ever returned.
#[cfg(target_os = "linux")]
fn timerfd_key(fd: libc::c_int) -> Option<usize> {
    usize::try_from(fd).ok()
}

/// Remembers that the POSIX timer `timer` was created on `clock`.
pub(crate) fn remember_timer(timer: libc::timer_t, clock: libc::clockid_t) {
    TIMERS.keep(timer_key(timer), clock);
}

/// Forgets the POSIX timer `timer`, which is what `timer_delete` does.
pub(crate) fn forget_timer(timer: libc::timer_t) {
    TIMERS.forget(timer_key(timer));
}

/// The clock `timer` was created on, or `None` when nothing remembers it: a timer created while
/// the table had no room, or one created where we could not see.
pub(crate) fn timer_clock(timer: libc::timer_t) -> Option<libc::clockid_t> {
    TIMERS.kept(timer_key(timer))
}

/// Remembers that the timerfd `fd` was created on `clock`.
#[cfg(target_os = "linux")]
pub(crate) fn remember_timerfd(fd: libc::c_int, clock: libc::clockid_t) {
    if let Some(key) = timerfd_key(fd) {
        TIMERFDS.keep(key, clock);
    }
}

/// The clock the timerfd `fd` counts: the kernel's answer, or where `/proc` cannot be read, the
/// clock the `timerfd_create` that handed out this number named. `None` when neither knows.
#[cfg(target_os = "linux")]
pub(crate) fn timerfd_clock(fd: libc::c_int) -> Option<libc::clockid_t> {
    described::clock(fd).or_else(|| TIMERFDS.kept(timerfd_key(fd)?))
}

/// The kernel's description of a descriptor, which for a timerfd names its clock.
#[cfg(target_os = "linux")]
mod described {
    /// Where the kernel describes the calling thread's descriptors, one file per descriptor. The
    /// thread's own rather than the process's, because a thread may have unshared its table.
    const DIRECTORY: &[u8] = b"/proc/thread-self/fdinfo/";

    /// The line of a timerfd's description that names its clock.
    const CLOCK_LINE: &[u8] = b"clockid:";

    /// The most of a description read. A timerfd's runs to a little over a hundred bytes, and its
    /// clock comes on the fifth line.
    const READ: usize = 512;

    /// The clock the kernel says `fd` counts, or `None` when its description cannot be read or
    /// names no clock, which is every descriptor but a timerfd.
    ///
    /// Async-signal-safe: an `open`, one `read` into the stack, and a `close`. The descriptor is
    /// opened and closed here, so it is never a number the program can see.
    pub(super) fn clock(fd: libc::c_int) -> Option<libc::clockid_t> {
        let mut path = [0u8; DIRECTORY.len() + 12];
        path_of(fd, &mut path)?;
        // Safety: `path` is NUL-terminated and lives across the call. Not a name we interpose.
        let file = unsafe { libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if file < 0 {
            return None;
        }
        let mut text = [0u8; READ];
        // Safety: `file` is open, and `text` is a live buffer of the length passed.
        let read = unsafe { libc::read(file, text.as_mut_ptr().cast(), text.len()) };
        // Safety: we opened this descriptor and nothing else holds it.
        unsafe { libc::close(file) };
        clock_in(text.get(..usize::try_from(read).ok()?)?)
    }

    /// Writes the NUL-terminated path of `fd`'s description into `path`, or `None` for a negative
    /// descriptor, which has none.
    fn path_of(fd: libc::c_int, path: &mut [u8]) -> Option<()> {
        let mut digits = [0u8; 10];
        let mut left = u32::try_from(fd).ok()?;
        let mut count = 0;
        loop {
            *digits.get_mut(count)? = b'0' + u8::try_from(left % 10).ok()?;
            count += 1;
            left /= 10;
            if left == 0 {
                break;
            }
        }
        let (head, tail) = path.split_at_mut_checked(DIRECTORY.len())?;
        head.copy_from_slice(DIRECTORY);
        for (to, from) in tail.iter_mut().zip(digits.get(..count)?.iter().rev()) {
            *to = *from;
        }
        *tail.get_mut(count)? = 0;
        Some(())
    }

    /// The clock a description names on its `clockid:` line.
    pub(super) fn clock_in(text: &[u8]) -> Option<libc::clockid_t> {
        let line = text
            .split(|&byte| byte == b'\n')
            .find_map(|line| line.strip_prefix(CLOCK_LINE))?;
        let number = line.trim_ascii();
        let (negative, digits) = match number.split_first() {
            Some((b'-', rest)) => (true, rest),
            _ => (false, number),
        };
        if digits.is_empty() {
            return None;
        }
        let mut value: libc::clockid_t = 0;
        for &digit in digits {
            if !digit.is_ascii_digit() {
                return None;
            }
            value = value
                .checked_mul(10)?
                .checked_add(libc::clockid_t::from(digit - b'0'))?;
        }
        Some(if negative { -value } else { value })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_path_names_the_descriptor_in_decimal() {
            let mut path = [0xFFu8; DIRECTORY.len() + 12];
            for (fd, name) in [
                (0, &b"0"[..]),
                (7, b"7"),
                (10, b"10"),
                (1_234, b"1234"),
                (libc::c_int::MAX, b"2147483647"),
            ] {
                path_of(fd, &mut path).unwrap();
                let end = path.iter().position(|&byte| byte == 0).unwrap();
                assert_eq!(&path[..DIRECTORY.len()], DIRECTORY);
                assert_eq!(&path[DIRECTORY.len()..end], name, "{fd}");
            }
            assert_eq!(path_of(-1, &mut path), None);
        }

        #[test]
        fn the_clock_is_read_off_its_own_line() {
            let timerfd = b"pos:\t0\nflags:\t02\nmnt_id:\t19\nino:\t2119\nclockid: 7\nticks: 0\n\
                            settime flags: 00\nit_value: (0, 0)\nit_interval: (0, 0)\n";
            assert_eq!(clock_in(timerfd), Some(7));
            assert_eq!(clock_in(b"clockid:\t1"), Some(1));
            assert_eq!(clock_in(b"clockid: -3\n"), Some(-3));
            // A descriptor that is not a timerfd has no such line.
            assert_eq!(
                clock_in(b"pos:\t0\nflags:\t02\nmnt_id:\t19\nino:\t1\n"),
                None
            );
            assert_eq!(clock_in(b"clockid: \n"), None);
            assert_eq!(clock_in(b"clockid: 1x\n"), None);
            assert_eq!(clock_in(b""), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use common::platform::clock;

    use super::*;

    /// The `which`th timer id, in the shape glibc hands them out: a small count from zero.
    fn timer(which: usize) -> libc::timer_t {
        std::ptr::without_provenance_mut(which)
    }

    #[test]
    fn a_timer_keeps_the_clock_it_was_created_on_until_it_is_deleted() {
        // Zero is the first id glibc hands out, so it has to be a key like any other.
        remember_timer(timer(0), clock::MONOTONIC);
        assert_eq!(timer_clock(timer(0)), Some(clock::MONOTONIC));
        // The default is kept like any other clock.
        remember_timer(timer(1), clock::REALTIME);
        assert_eq!(timer_clock(timer(1)), Some(clock::REALTIME));
        // And one nobody created has no clock to answer with.
        assert_eq!(timer_clock(timer(7)), None);

        forget_timer(timer(0));
        assert_eq!(timer_clock(timer(0)), None);
        assert_eq!(timer_clock(timer(1)), Some(clock::REALTIME));
    }

    #[test]
    fn a_timer_created_and_deleted_over_and_over_leaves_room_for_the_next() {
        for which in 1_000..30_000 {
            remember_timer(timer(which), clock::MONOTONIC);
            forget_timer(timer(which));
        }
        remember_timer(timer(30_000), clock::MONOTONIC);
        assert_eq!(timer_clock(timer(30_000)), Some(clock::MONOTONIC));
    }

    /// A timerfd the kernel made, through the system call rather than through any name of ours.
    #[cfg(target_os = "linux")]
    fn genuine_timerfd(clock: libc::clockid_t) -> libc::c_int {
        // Safety: the call takes two plain values.
        let fd = unsafe { libc::syscall(libc::SYS_timerfd_create, clock, 0) };
        libc::c_int::try_from(fd).unwrap()
    }

    /// A descriptor's number handed out again, the way `dup` and its neighbours do.
    #[cfg(target_os = "linux")]
    fn duplicated(fd: libc::c_int) -> libc::c_int {
        // Safety: the call takes a plain value and touches no memory.
        let copy = unsafe { libc::syscall(libc::SYS_dup, fd) };
        libc::c_int::try_from(copy).unwrap()
    }

    #[cfg(target_os = "linux")]
    fn close(fd: libc::c_int) {
        // Safety: the descriptor is this test's own.
        unsafe { libc::syscall(libc::SYS_close, fd) };
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_timerfd_reads_its_clock_off_the_kernel_whatever_number_it_has() {
        let made = genuine_timerfd(clock::BOOTTIME);
        let copy = duplicated(made);
        assert_eq!(timerfd_clock(made), Some(clock::BOOTTIME));
        assert_eq!(timerfd_clock(copy), Some(clock::BOOTTIME));
        close(made);
        assert_eq!(timerfd_clock(copy), Some(clock::BOOTTIME));

        // A number kept for a timerfd of another clock gives way to the kernel's answer.
        remember_timerfd(copy, clock::MONOTONIC);
        assert_eq!(timerfd_clock(copy), Some(clock::BOOTTIME));
        close(copy);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_descriptor_that_is_no_timerfd_has_no_clock_of_its_own() {
        // Safety: the call takes two plain values.
        let file = unsafe { libc::syscall(libc::SYS_eventfd2, 0, 0) };
        let file = libc::c_int::try_from(file).unwrap();
        assert_eq!(timerfd_clock(file), None);
        close(file);
        // A descriptor no `timerfd_create` returned has no description at all.
        assert_eq!(timerfd_clock(-1), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_timer_and_a_timerfd_of_the_same_number_are_different_objects() {
        remember_timerfd(9_999, clock::MONOTONIC);
        assert_eq!(timer_clock(timer(9_999)), None);
        // Nothing is open at that number, so the kept clock is the answer.
        assert_eq!(timerfd_clock(9_999), Some(clock::MONOTONIC));
    }
}
