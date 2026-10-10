//! Transactions against a live session: what staging writes, which is nothing; what a commit
//! publishes, which is one section for the whole transaction; and what each family refuses.
//!
//! Creating a session reads a genuine clock and the system's randomness, so these cases need a
//! filled-in platform arm.
#![cfg(any(target_os = "linux", target_os = "freebsd"))]
#![expect(
    clippy::unwrap_used,
    reason = "test support: a failed assumption should fail the test loudly"
)]
#![expect(
    clippy::significant_drop_tightening,
    reason = "a transaction is consumed by `commit` or `abort`, which is where its lock is \
              released; the lint does not see through a method taking `self` and suggests a drop \
              that would not compile"
)]

mod fixture;

use std::sync::atomic::{AtomicBool, Ordering};

use common::platform::clock;
use common::session::layout::{Header, Keeper, Params, ParamsValue};
use common::session::naming;
use common::sync::read;
use common::time::{Family, Nanos, Rate};
use controller::{Controller, Error, Transaction};

use fixture::{HOUR, SECOND, fixture, fixture_on, genuine, one_at_a_time, timebase_of};

/// Runs `body` with a thread reading the parameters through the seqlock the whole time, and
/// asserts that every copy it read was `expected`.
///
/// What a reader must never see is any part of a section that wrote nothing: an abort leaves the
/// live words untouched and the shadow-complete flag clear, which to a reader is a writer that
/// died between **W3** and **W7**.
#[track_caller]
fn while_reading<T>(header: &Header, expected: ParamsValue, body: impl FnOnce() -> T) -> T {
    let reading = AtomicBool::new(false);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut reads: u64 = 0;
            let mut whole = true;
            reading.store(true, Ordering::Release);
            while !stop.load(Ordering::Relaxed) {
                whole &= read(header, Params::load) == expected;
                reads += 1;
            }
            (whole, reads)
        });
        // The section is a few tens of microseconds, which is shorter than starting a thread, so
        // the reader has to be going before the section opens for it to span one.
        while !reading.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
        let value = body();
        stop.store(true, Ordering::Relaxed);
        let (whole, reads) = reader.join().unwrap();
        assert!(whole, "a reader saw something the section never published");
        assert!(reads > 10, "the reader got {reads} reads in");
        value
    })
}

/// Asserts that `value` lies in the window a genuine clock moved through while the commit ran,
/// shifted by `by`.
#[track_caller]
fn within(value: Nanos, window: (Nanos, Nanos), by: i64) {
    let (low, high) = window;
    assert!(
        value >= Nanos(low.0 + by) && value <= Nanos(high.0 + by),
        "{value:?} is not {by} ns past {low:?}..={high:?}",
    );
}

#[test]
fn attaching_applies_the_checks_a_target_applies() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let dir = fixture.dir();

    let missing = dir.join("pid-1-nothing.session");
    assert!(matches!(
        Controller::attach(&missing),
        Err(Error::NoSession)
    ));

    let garbage = dir.join("pid-2-garbage.session");
    std::fs::write(&garbage, vec![0u8; 64 * 1024]).unwrap();
    assert!(matches!(
        Controller::attach(&garbage),
        Err(Error::BadSessionFile(_))
    ));

    // A planted FIFO is not a regular file, so it is refused rather than opened and read.
    let fifo = dir.join("pid-3-fifo.session");
    let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // Safety: `c_fifo` is a live NUL-terminated path in a directory of ours.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    assert!(matches!(Controller::attach(&fifo), Err(Error::NotOurs(_))));
}

#[test]
fn begin_takes_the_lock_and_leaves_everything_else_alone() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let before = fixture.live();
    let counter = fixture.counter();

    let transaction = fixture.controller.begin().unwrap();
    // The counter stays even, so readers keep reading the old parameters, and it has not moved at
    // all, so nothing has been published either.
    assert_eq!(fixture.counter(), counter);
    assert_eq!(counter % 2, 0);
    assert_eq!(fixture.live(), before);
    assert_eq!(
        fixture
            .header()
            .seqlock
            .writer_state
            .load(Ordering::Acquire),
        0
    );
    transaction.abort();
}

#[test]
fn staging_writes_nothing_and_the_commit_publishes_once() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let before = fixture.live();
    let counter = fixture.counter();

    let mut transaction = fixture.controller.begin().unwrap();
    transaction.shift(Family::Realtime, Nanos(HOUR)).unwrap();
    transaction
        .set_time(Family::Boottime, Nanos(i64::MAX / 2))
        .unwrap();
    transaction.set_rate(Family::Monotonic, 4.0).unwrap();
    assert_eq!(fixture.live(), before, "staging wrote to the session");
    assert_eq!(fixture.counter(), counter);

    transaction.commit().unwrap();
    assert_eq!(fixture.counter(), counter + 2, "one section, one publish");
    assert_ne!(fixture.live(), before);
}

#[test]
fn a_transaction_touching_three_families_samples_each_timebase_inside_the_one_section() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let counter = fixture.counter();
    // Each family tracks a timebase of its own in a plain session, so the anchors below are three
    // different clocks read inside one section.
    let live = fixture.live();
    assert_eq!(live.realtime.timebase, clock::REALTIME);
    assert_eq!(live.monotonic.timebase, timebase_of(Family::Monotonic));
    assert_eq!(live.boottime.timebase, timebase_of(Family::Boottime));

    let before = [
        genuine(clock::REALTIME),
        genuine(timebase_of(Family::Monotonic)),
        genuine(timebase_of(Family::Boottime)),
    ];
    let mut transaction = fixture.controller.begin().unwrap();
    transaction.shift(Family::Realtime, Nanos(HOUR)).unwrap();
    transaction
        .shift(Family::Monotonic, Nanos(10 * SECOND))
        .unwrap();
    transaction.set_rate(Family::Boottime, 3.0).unwrap();
    transaction.commit().unwrap();
    let after = [
        genuine(clock::REALTIME),
        genuine(timebase_of(Family::Monotonic)),
        genuine(timebase_of(Family::Boottime)),
    ];

    assert_eq!(
        fixture.counter(),
        counter + 2,
        "three families, one publish"
    );
    let next = fixture.live();
    for (index, family) in next.lines().into_iter().enumerate() {
        within(
            family.line.timebase_anchor,
            (before[index], after[index]),
            0,
        );
    }
    within(next.realtime.line.fake_anchor, (before[0], after[0]), HOUR);
    within(
        next.monotonic.line.fake_anchor,
        (before[1], after[1]),
        10 * SECOND,
    );
    assert_eq!(next.boottime.line.rate, Rate::from_f64(3.0).unwrap());
}

#[test]
fn the_staged_operations_apply_in_order_against_the_anchor() {
    let _one = one_at_a_time();
    let fixture = fixture();

    let before = genuine(clock::REALTIME);
    let mut transaction = fixture.controller.begin().unwrap();
    transaction
        .shift(Family::Realtime, Nanos(10 * SECOND))
        .unwrap();
    transaction.set_rate(Family::Realtime, 2.0).unwrap();
    transaction
        .shift(Family::Realtime, Nanos(5 * SECOND))
        .unwrap();
    transaction.commit().unwrap();
    let after = genuine(clock::REALTIME);

    // The second shift lands on what the first one left, so the family moved by the sum, and the
    // rate change in between neither moved the value nor was undone by the shift after it.
    let live = fixture.live();
    within(live.realtime.line.fake_anchor, (before, after), 15 * SECOND);
    assert_eq!(live.realtime.line.rate, Rate::from_f64(2.0).unwrap());
}

#[test]
fn monotonic_and_boottime_refuse_a_move_backwards() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let before = fixture.live();

    for (family, clock) in [
        (Family::Monotonic, timebase_of(Family::Monotonic)),
        (Family::Boottime, timebase_of(Family::Boottime)),
    ] {
        let mut transaction = fixture.controller.begin().unwrap();
        transaction
            .set_time(family, fixture.showing(clock).saturating_sub(Nanos(SECOND)))
            .unwrap();
        match transaction.commit() {
            Err(Error::WouldGoBackwards(refused)) => assert_eq!(refused, family),
            other => panic!("{family} took a value it had already shown: {other:?}"),
        }

        let mut transaction = fixture.controller.begin().unwrap();
        transaction.shift(family, Nanos(-1)).unwrap();
        assert!(matches!(
            transaction.commit(),
            Err(Error::WouldGoBackwards(_))
        ));

        // A move forward is what they do take.
        let mut transaction = fixture.controller.begin().unwrap();
        transaction.shift(family, Nanos(SECOND)).unwrap();
        transaction.commit().unwrap();
    }
    assert_ne!(fixture.live(), before);
}

#[test]
fn a_refused_operation_aborts_the_whole_transaction_with_nothing_written() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let before = fixture.live();
    let shadow = fixture.header().shadow.load();
    let counter = fixture.counter();

    let mut transaction = fixture.controller.begin().unwrap();
    transaction.shift(Family::Realtime, Nanos(HOUR)).unwrap();
    transaction.shift(Family::Monotonic, Nanos(-1)).unwrap();
    // The readers spinning through the aborted section are the point: an abort has to look to them
    // like a writer that died between W3 and W7, which is a writer that published nothing.
    let refused = while_reading(fixture.header(), before, || transaction.commit());
    assert!(matches!(
        refused,
        Err(Error::WouldGoBackwards(Family::Monotonic))
    ));

    // The Realtime shift staged before the refused one is gone with it, and so is everything the
    // section would have written: W6 through W9 never ran.
    assert_eq!(fixture.live(), before);
    assert_eq!(fixture.header().shadow.load(), shadow);
    // The section opened and closed all the same, which is what makes the abort look to a reader
    // exactly like a writer that died between W3 and W7.
    assert_eq!(fixture.counter(), counter + 2);
    assert_eq!(
        fixture
            .header()
            .seqlock
            .writer_state
            .load(Ordering::Acquire),
        0
    );
}

#[test]
fn realtime_takes_any_value_inside_its_range() {
    let _one = one_at_a_time();
    let fixture = fixture();

    // The wall clock may be set to a value it has already shown; only the nondecreasing families
    // refuse that.
    let mut transaction = fixture.controller.begin().unwrap();
    transaction.set_time(Family::Realtime, Nanos(0)).unwrap();
    transaction.commit().unwrap();
    assert_eq!(fixture.live().realtime.line.fake_anchor, Nanos(0));
    assert!(fixture.showing(clock::REALTIME) < Nanos(SECOND));
}

#[test]
fn a_move_out_of_the_range_an_instant_holds_is_refused() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let before = fixture.live();

    let mut transaction = fixture.controller.begin().unwrap();
    transaction
        .set_time(Family::Realtime, Nanos(i64::MAX - 1_000))
        .unwrap();
    transaction.shift(Family::Realtime, Nanos(SECOND)).unwrap();
    assert!(matches!(transaction.commit(), Err(Error::BadArgument(_))));
    assert_eq!(fixture.live(), before);
}

#[test]
fn cpu_refuses_every_operation() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let before = fixture.live();
    let mut transaction = fixture.controller.begin().unwrap();

    // Its rate is fixed at creation, because a writer in another process cannot read the target
    // thread's own CPU clock, and it has no single value to set either.
    assert!(matches!(
        transaction.set_rate(Family::Cpu, 2.0),
        Err(Error::BadArgument(_))
    ));
    assert!(matches!(
        transaction.shift(Family::Cpu, Nanos(SECOND)),
        Err(Error::BadArgument(_))
    ));
    assert!(matches!(
        transaction.set_time(Family::Cpu, Nanos(SECOND)),
        Err(Error::BadArgument(_))
    ));

    // A refusal at staging stages nothing, so the transaction is still the caller's to use.
    transaction.set_rate(Family::Realtime, 2.0).unwrap();
    transaction.commit().unwrap();
    let live = fixture.live();
    assert_eq!(live.cpu_rate, before.cpu_rate);
    assert_eq!(live.realtime.line.rate, Rate::from_f64(2.0).unwrap());
}

#[test]
fn a_rate_crosses_the_edge_as_a_double() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let mut transaction = fixture.controller.begin().unwrap();

    for refused in [-1.0, -0.0001, f64::NAN, f64::INFINITY, 4.3e9, 1e30] {
        assert!(
            matches!(
                transaction.set_rate(Family::Realtime, refused),
                Err(Error::BadArgument(_))
            ),
            "{refused} was taken as a rate",
        );
    }

    // And what is taken converts to the fixed point at this edge, exactly as the conversion the
    // environment's own rates go through.
    transaction.set_rate(Family::Realtime, 2.5).unwrap();
    transaction.set_rate(Family::Monotonic, 0.0).unwrap();
    transaction.commit().unwrap();
    let live = fixture.live();
    assert_eq!(live.realtime.line.rate, Rate::from_f64(2.5).unwrap());
    assert_eq!(live.monotonic.line.rate, Rate::FROZEN);

    // A frozen family stays where the commit left it, however much genuine time passes.
    let frozen = fixture.showing(timebase_of(Family::Monotonic));
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(fixture.showing(timebase_of(Family::Monotonic)), frozen);
}

#[test]
fn a_null_session_takes_no_changes() {
    let _one = one_at_a_time();
    let fixture = fixture_on(Keeper::Null);
    assert!(matches!(
        fixture.controller.begin(),
        Err(Error::UnsupportedForKeeper)
    ));
    // Nothing was written, and nothing was locked either: the session is as the creator left it.
    assert_eq!(fixture.counter() % 2, 0);
}

#[test]
fn a_transaction_opens_over_a_session_the_caller_already_holds_mapped() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let lock_path = naming::lock_path(fixture.session.path());
    let before = fixture.counter();

    // The same W1 a controller takes, over a header somebody else mapped, which is how a target
    // that sets the clock becomes a writer without opening a second view of its own session.
    let mut transaction = Transaction::begin(fixture.header(), &lock_path).unwrap();
    assert_eq!(fixture.counter(), before, "begin publishes nothing");
    transaction.shift(Family::Realtime, Nanos(HOUR)).unwrap();
    transaction.commit().unwrap();
    assert_eq!(fixture.counter(), before + 2, "one publish, not two");

    // And a null session refuses at that entry point too, before it locks anything.
    let null = fixture_on(Keeper::Null);
    assert!(matches!(
        Transaction::begin(null.header(), &naming::lock_path(null.session.path())),
        Err(Error::UnsupportedForKeeper)
    ));
}

#[test]
fn abort_discards_the_staged_operations_and_releases_the_lock() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let before = fixture.live();
    let counter = fixture.counter();

    let mut transaction = fixture.controller.begin().unwrap();
    transaction.shift(Family::Realtime, Nanos(HOUR)).unwrap();
    transaction.set_rate(Family::Boottime, 9.0).unwrap();
    transaction.abort();

    // No section ever opened, so the counter did not move at all.
    assert_eq!(fixture.counter(), counter);
    assert_eq!(fixture.live(), before);

    // And the lock is free, so the next transaction takes it without waiting.
    fixture.controller.begin().unwrap().abort();
}

#[test]
fn a_second_writer_waits_for_the_transaction_to_finish() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let path = fixture.path();
    let go = Pipe::new();
    let took_it = Pipe::new();

    // Forked before the lock is taken: a child of a process already inside a transaction inherits
    // the in-process half of the write lock held, and would wait on that rather than on the file.
    let second = fork_child(|| {
        go.wait();
        let controller = Controller::attach(&path).unwrap();
        let transaction = controller.begin().unwrap();
        took_it.ring();
        transaction.abort();
        0
    });

    let mut transaction = fixture.controller.begin().unwrap();
    transaction.shift(Family::Realtime, Nanos(HOUR)).unwrap();
    go.ring();
    assert!(
        !took_it.rang_within(200),
        "the second writer did not wait for the first"
    );
    transaction.commit().unwrap();

    took_it.wait();
    assert_eq!(reap(second), 0);
}

#[test]
fn a_controller_dying_between_begin_and_commit_leaves_nothing_behind() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let path = fixture.path();
    let before = fixture.live();
    let counter = fixture.counter();

    let doorbell = Pipe::new();
    let dying = fork_child(|| {
        let controller = Controller::attach(&path).unwrap();
        let mut transaction = controller.begin().unwrap();
        transaction.shift(Family::Realtime, Nanos(HOUR)).unwrap();
        doorbell.ring();
        wait_to_be_killed()
    });
    doorbell.wait();
    assert_eq!(fixture.counter(), counter, "W2 ran before the commit");
    kill_and_reap(dying);

    // The kernel released the lock with the last reference to the description, so the next
    // controller takes it at once and finds a seqlock with nothing to repair.
    let transaction = fixture.controller.begin().unwrap();
    assert_eq!(fixture.counter(), counter);
    assert_eq!(fixture.live(), before);
    assert_eq!(
        fixture
            .header()
            .seqlock
            .writer_state
            .load(Ordering::Acquire),
        0
    );
    transaction.abort();
}

/// One byte in one direction, for a child to say where it has got to.
struct Pipe {
    read_end: libc::c_int,
    write_end: libc::c_int,
}

impl Pipe {
    fn new() -> Self {
        let mut ends = [0; 2];
        // Safety: `ends` is a live pair of descriptors for `pipe` to fill.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        Self {
            read_end: ends[0],
            write_end: ends[1],
        }
    }

    fn ring(&self) {
        let byte = 1u8;
        // Safety: the descriptor is ours and the buffer is one live byte.
        unsafe { libc::write(self.write_end, std::ptr::from_ref(&byte).cast(), 1) };
    }

    fn wait(&self) {
        let mut byte = 0u8;
        // Safety: the descriptor is ours and the buffer is one live byte.
        let got = unsafe { libc::read(self.read_end, std::ptr::from_mut(&mut byte).cast(), 1) };
        assert_eq!(got, 1, "nobody rang");
    }

    /// Whether anything has been written within `millis`, taking nothing off the pipe.
    fn rang_within(&self, millis: libc::c_int) -> bool {
        let mut watched = libc::pollfd {
            fd: self.read_end,
            events: libc::POLLIN,
            revents: 0,
        };
        // Safety: `watched` is a live `pollfd` on a descriptor of ours.
        unsafe { libc::poll(&raw mut watched, 1, millis) > 0 }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        for fd in [self.read_end, self.write_end] {
            // Safety: both descriptors are ours and nothing else holds them.
            unsafe { libc::close(fd) };
        }
    }
}

/// Runs `body` in a child process, which leaves through `_exit` with whatever it returned: a panic
/// there would unwind into a test harness that belongs to the parent.
fn fork_child(body: impl FnOnce() -> i32) -> libc::pid_t {
    // Safety: `fork` is called from a case holding `ONE_AT_A_TIME`, so no other case of this file
    // is inside a transaction.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
    if pid > 0 {
        return pid;
    }
    let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or(101);
    // Safety: `_exit` ends the child, so nothing here can be reached afterwards.
    unsafe { libc::_exit(code) }
}

fn reap(pid: libc::pid_t) -> i32 {
    let mut status = 0;
    // Safety: `pid` is a child of ours and `status` is a live buffer.
    unsafe { libc::waitpid(pid, &raw mut status, 0) };
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        128 + libc::WTERMSIG(status)
    }
}

/// Kills `pid` and waits for the kernel to finish with it, so the write lock it held is released
/// before the caller looks.
fn kill_and_reap(pid: libc::pid_t) {
    // Safety: `pid` is a child of ours.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    assert_eq!(reap(pid), 128 + libc::SIGKILL);
}

fn wait_to_be_killed() -> ! {
    loop {
        // Safety: `pause` waits for a signal and touches nothing of ours.
        unsafe { libc::pause() };
    }
}

#[test]
fn a_move_of_the_value_bumps_the_epoch_and_a_change_of_speed_leaves_it() {
    let _one = one_at_a_time();
    let fixture = fixture();
    let started = fixture.live().epoch;

    // A rate change leaves the line where it is, so the page's republisher has continuity to
    // enforce rather than a step to follow.
    let mut transaction = fixture.controller.begin().unwrap();
    transaction.set_rate(Family::Realtime, 2.0).unwrap();
    transaction.set_rate(Family::Monotonic, 2.0).unwrap();
    transaction.set_rate(Family::Boottime, 2.0).unwrap();
    transaction.commit().unwrap();
    assert_eq!(fixture.live().epoch, started, "a rate change bumped it");

    // A move of the value is the user's own discontinuity, and the epoch is what says so.
    let mut transaction = fixture.controller.begin().unwrap();
    transaction.shift(Family::Realtime, Nanos(HOUR)).unwrap();
    transaction.commit().unwrap();
    let shifted = fixture.live().epoch;
    assert_ne!(shifted, started, "a shift left the epoch alone");

    let mut transaction = fixture.controller.begin().unwrap();
    transaction
        .set_time(Family::Realtime, Nanos(SECOND))
        .unwrap();
    transaction.commit().unwrap();
    assert_ne!(fixture.live().epoch, shifted, "a set_time left it alone");
}
