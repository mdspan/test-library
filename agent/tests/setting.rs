//! A target that sets the time: the clock moves for every process of the session, and a program
//! that probes for privilege by calling a setter sees exactly what it would have seen without us.
//!
//! A case runs its target with or without the privilege the kernel asks for, and no target may
//! ever move the machine's clock. A privileged target runs behind a guard that refuses every
//! genuine set: on Linux a seccomp filter around this process's own privilege or `CAP_SYS_TIME`
//! that `sudo` hands over, and on macOS a sandbox around root. FreeBSD has no such guard, so a
//! target there never holds the privilege, and a process that does runs its targets in a jail.
//! Where a case cannot have the target it wants it says so on stderr and checks the refusal, and
//! `TIMEWARP_TEST_SETTERS=require` makes that a failure instead.
//!
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "test support: a failed assumption should fail the test loudly, and not every helper \
              is used by every case"
)]
#![expect(
    clippy::print_stderr,
    reason = "a case that cannot get the capability it wanted says so, rather than passing quietly"
)]

mod harness;

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use common::hook::{Func, Kind};
use common::platform::{clock, genuine_nanos, may_set_the_clock};
use common::session::Session;
use harness::{Report, Run, arena_words, only_session};

const SECOND: i64 = 1_000_000_000;
const DAY: i64 = 86_400 * SECOND;

/// The instant every case here asks the wall clock to show: 2020-01-01T00:00:00Z. Far enough from
/// today that a clock sitting at it cannot be one that never moved.
const TARGET: i64 = 1_577_836_800 * SECOND;

/// How far the one adjustment that is meant to land moves the clock, and how far ahead of `TARGET`
/// the last setter puts it. Both are the numbers `setters.c` uses, checked against it on every run.
const ADJUST_S: i64 = 600;
const LAST_S: i64 = 7_200;

/// How close a reading has to be to count as the same instant. A second, which is longer than any
/// of these runs.
const SLACK: i64 = SECOND;

/// The arena a hook logging every call is given, and how many words each of its entries takes.
const ARENA: u64 = 4096;
const SETTER_ENTRY: usize = 4;
const DECLINED_ENTRY: usize = 3;

/// What a case runs its target under: the command that execs it, if any, and what that hands the
/// target.
///
/// No target here may set the machine's clock, even where the library under test fails to take a
/// setter over. A target the kernel lets set the clock therefore runs behind a guard that refuses
/// every genuine set before the kernel looks at it, and is used only once a check made through the
/// same command has seen that guard refuse.
#[derive(Debug, Clone)]
struct Target {
    /// The command and its arguments, which the target's own command line follows.
    wrapper: Vec<String>,
    /// Whether the command runs what it execs with an environment of its own, `sudo` for one, so
    /// the target's environment has to follow the command as arguments.
    resets_env: bool,
    /// Whether the kernel lets the target set the clock.
    privileged: bool,
    /// Whether the target runs as another user, whose session files this process cannot read.
    other_user: bool,
}

impl Target {
    /// The target run as it is, holding whatever this process holds.
    const fn direct(privileged: bool) -> Self {
        Self {
            wrapper: Vec::new(),
            resets_env: false,
            privileged,
            other_user: false,
        }
    }

    /// One run of `prog` under this target, its sessions landing in `dir`.
    fn run(&self, prog: &str, dir: &SessionDir) -> Run {
        let run = Run::prog(prog).session_dir(dir.path());
        if self.resets_env {
            run.through_resetting(&self.wrapper)
        } else {
            run.through(&self.wrapper)
        }
    }
}

/// The directory a case's sessions land in. A target running as another user leaves files there
/// that only that user can remove, and `sudo` removes them with the directory.
struct SessionDir {
    dir: tempfile::TempDir,
    other_user: bool,
}

impl SessionDir {
    fn for_target(target: &Target) -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            other_user: target.other_user,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for SessionDir {
    fn drop(&mut self) {
        if self.other_user {
            let _removed = Command::new("sudo")
                .args(["-n", "rm", "-rf", "--"])
                .arg(self.dir.path().join("libtimewarp"))
                .stdin(Stdio::null())
                .status();
        }
    }
}

/// The variable that makes a case fail rather than fall back when this machine cannot give its
/// target what the case wants. CI sets it to `require` on the machines that can.
const STRICT: &str = "TIMEWARP_TEST_SETTERS";

/// Whether this platform has a guard that lets a target hold the privilege at all; see
/// [`guarded_privilege`].
const GUARD_EXISTS: bool = !cfg!(target_os = "freebsd");

/// Says that a case runs `what` as an unprivileged process, because `why`, or fails the case where
/// that was not to be allowed.
fn fall_back(why: &str, what: &str) {
    let message =
        format!("{why}, so {what} runs as an unprivileged process and checks the refusal");
    assert!(
        !GUARD_EXISTS || std::env::var(STRICT).as_deref() != Ok("require"),
        "{message}; {STRICT}=require forbids that"
    );
    eprintln!("{message}");
}

/// Whether `command` runs and succeeds, which is how a wrapper is checked before a case uses it.
fn runs(command: &[&str]) -> bool {
    let Some((program, args)) = command.split_first() else {
        return false;
    };
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn owned(command: &[&str]) -> Vec<String> {
    command.iter().map(|&part| part.to_owned()).collect()
}

/// A target the kernel lets set the clock, behind a guard, or why this machine has none.
fn privileged() -> Result<&'static Target, &'static str> {
    static FOUND: OnceLock<Result<Target, String>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let target = guarded_privilege()?;
            let check = Run::prog("guard_check").without_library();
            let check = if target.resets_env {
                check.through_resetting(&target.wrapper)
            } else {
                check.through(&target.wrapper)
            };
            let report = check.run_unchecked();
            if !report.status.success() || !report.has("guarded") {
                return Err(format!(
                    "`{}` could not run the guard check ({}: {})",
                    target.wrapper.join(" "),
                    report.status,
                    report.stderr.trim()
                ));
            }
            if report.nanos("privileged") != 1 {
                return Err(format!(
                    "`{}` hands the target no privilege to set the clock",
                    target.wrapper.join(" ")
                ));
            }
            if report.nanos("guarded") != 1 {
                return Err(format!(
                    "`{}` hands the target privilege without refusing the genuine sets",
                    target.wrapper.join(" ")
                ));
            }
            Ok(target)
        })
        .as_ref()
        .map_err(String::as_str)
}

/// The command that would hand a target genuine privilege behind a guard, still to be checked.
///
/// On Linux the guard is `clock_guard`'s seccomp filter, and the privilege is this process's own
/// or `CAP_SYS_TIME` that `sudo` and `setpriv` hand a target running as this same user. On macOS
/// the guard is a sandbox that denies `system-set-time`, and the privilege is root, this
/// process's own or `sudo`'s. FreeBSD has nothing that refuses a genuine set to a process the
/// kernel lets set the clock, so a target there never holds the privilege.
fn guarded_privilege() -> Result<Target, String> {
    #[cfg(target_os = "linux")]
    {
        let guard = harness::program("clock_guard")
            .to_string_lossy()
            .into_owned();
        if may_set_the_clock() {
            return Ok(Target {
                wrapper: vec![guard],
                resets_env: false,
                privileged: true,
                other_user: false,
            });
        }
        if !runs(&["sudo", "-n", "true"]) {
            return Err("this process may not set the clock and `sudo -n` will not run".into());
        }
        // Safety: `getuid` reads process state and cannot fail.
        let uid = unsafe { libc::getuid() };
        // Safety: as above, for `getgid`.
        let gid = unsafe { libc::getgid() };
        let mut wrapper = owned(&["sudo", "-n", "setpriv"]);
        wrapper.extend([format!("--reuid={uid}"), format!("--regid={gid}")]);
        wrapper.extend(owned(&[
            "--init-groups",
            "--inh-caps=+sys_time",
            "--ambient-caps=+sys_time",
            "--",
        ]));
        wrapper.push(guard);
        Ok(Target {
            wrapper,
            resets_env: true,
            privileged: true,
            other_user: false,
        })
    }
    #[cfg(target_os = "macos")]
    {
        let sandbox = [
            "/usr/bin/sandbox-exec",
            "-p",
            "(version 1)(allow default)(deny system-set-time)",
        ];
        if may_set_the_clock() {
            return Ok(Target {
                wrapper: owned(&sandbox),
                resets_env: true,
                privileged: true,
                other_user: false,
            });
        }
        if !runs(&["sudo", "-n", "true"]) {
            return Err("this process is not root and `sudo -n` will not run".into());
        }
        let mut wrapper = owned(&["sudo", "-n"]);
        wrapper.extend(owned(&sandbox));
        Ok(Target {
            wrapper,
            resets_env: true,
            privileged: true,
            other_user: true,
        })
    }
    #[cfg(target_os = "freebsd")]
    Err(
        "FreeBSD has nothing that refuses a genuine set to a process the kernel lets set the clock"
            .into(),
    )
}

/// A target the kernel would not let set the clock, or why this machine has none: this process
/// as it is where it holds no privilege, and otherwise what [`confined`] makes of it.
fn unprivileged() -> Result<&'static Target, &'static str> {
    static FOUND: OnceLock<Result<Target, String>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            if may_set_the_clock() {
                confined().cloned().map_err(str::to_owned)
            } else {
                Ok(Target::direct(false))
            }
        })
        .as_ref()
        .map_err(String::as_str)
}

/// A target running as root of a confinement the kernel does not let set the clock, or why this
/// machine has none: a user namespace of its own on Linux, which hands it every capability inside
/// and none the kernel checks a set against, and a jail on FreeBSD, whose root has no privilege to
/// set the clock unless the jail is made with `allow.settime`. macOS has neither.
fn confined() -> Result<&'static Target, &'static str> {
    static FOUND: OnceLock<Result<Target, String>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            #[cfg(target_os = "linux")]
            let (wrapper, check, missing) = (
                &["unshare", "-U", "-r"][..],
                &["unshare", "-U", "-r", "true"][..],
                "this machine will not make a user namespace for this user",
            );
            // A target's own search path is `/usr/bin:/bin`, which `jail` is not on.
            #[cfg(target_os = "freebsd")]
            let (wrapper, check, missing) = (
                &["/usr/sbin/jail", "-c", "path=/", "command=/usr/bin/env"][..],
                &["/usr/sbin/jail", "-c", "path=/", "command=/usr/bin/true"][..],
                "this process may not make a jail",
            );
            #[cfg(target_os = "macos")]
            let (wrapper, check, missing): (&[&str], &[&str], _) =
                (&[], &[], "macOS has neither user namespaces nor jails");
            if check.is_empty() || !runs(check) {
                return Err(missing.to_owned());
            }
            Ok(Target {
                wrapper: owned(wrapper),
                resets_env: false,
                privileged: false,
                other_user: false,
            })
        })
        .as_ref()
        .map_err(String::as_str)
}

/// The privileged target, or the unprivileged one with a line saying what goes unexercised, or
/// `None` with a line saying the case is not run at all.
fn privileged_or_fall_back(what: &str) -> Option<&'static Target> {
    match privileged() {
        Ok(target) => Some(target),
        Err(why) => {
            fall_back(why, what);
            unprivileged_or_skip(what)
        }
    }
}

/// The privileged target where this process can read the sessions it leaves, or the unprivileged
/// one with a line saying what goes unexercised, or `None` with a line saying the case is not run.
fn privileged_as_this_user_or_fall_back(what: &str) -> Option<&'static Target> {
    match privileged() {
        Ok(target) if !target.other_user => Some(target),
        Ok(_) => {
            eprintln!(
                "the privileged target runs as another user, whose session this process cannot \
                 read, so {what} runs as an unprivileged process and checks the refusal"
            );
            unprivileged_or_skip(what)
        }
        Err(why) => {
            fall_back(why, what);
            unprivileged_or_skip(what)
        }
    }
}

/// Either target, for a case whose answer does not turn on privilege, as long as this process can
/// read the sessions it leaves; or `None` with a line saying the case is not run.
fn any_target_as_this_user(what: &str) -> Option<&'static Target> {
    let target = unprivileged()
        .or_else(|_| privileged())
        .ok()
        .filter(|target| !target.other_user);
    if target.is_none() {
        eprintln!("no target here runs as this user, so {what} is not run");
    }
    target
}

/// The unprivileged target, or `None` with a line saying the case is not run.
fn unprivileged_or_skip(what: &str) -> Option<&'static Target> {
    unprivileged()
        .inspect_err(|why| skip(&format!("this process may set the clock and {why}"), what))
        .ok()
}

/// Says that a case is not run, because `why`, or fails it where that was not to be allowed.
fn skip(why: &str, what: &str) {
    let message = format!("{why}, so {what} is not run");
    assert!(
        std::env::var(STRICT).as_deref() != Ok("require"),
        "{message}; {STRICT}=require forbids that"
    );
    eprintln!("{message}");
}

/// Every target this machine has, privileged and unprivileged.
fn every_target() -> Vec<&'static Target> {
    let mut targets = Vec::new();
    match privileged() {
        Ok(target) => targets.push(target),
        Err(why) => fall_back(why, "the case that runs every kind of target"),
    }
    match unprivileged() {
        Ok(target) => targets.push(target),
        Err(why) => skip(
            &format!("this process may set the clock and {why}"),
            "the unprivileged half of the case that runs every kind of target",
        ),
    }
    targets
}

/// Whether a run that reaches the platform's own setters may pass a null pointer to them.
///
/// Linux and FreeBSD make each of these a call into the kernel, which answers `EFAULT` for a
/// pointer it cannot read and takes a null delta as the question it is. macOS builds
/// `clock_settime` out of `settimeofday` inside libc and reads the pointer on the way, so a
/// genuine run there would fault where the other two are refused. Under the library the answer is
/// ours on all three, so every faked run makes them.
const GENUINE_NULLS: bool = !cfg!(target_os = "macos");

/// One run of the program that tries every setter, under whatever target and environment a case
/// wants. The library answers the calls that pass a null pointer, so a faked run makes them.
fn setters(target: &Target, dir: &SessionDir, env: &[(&str, &str)]) -> Report {
    run_setters(target, dir, env, true)
}

/// The run itself, which decides whether the calls that pass a null pointer are made at all.
fn run_setters(target: &Target, dir: &SessionDir, env: &[(&str, &str)], nulls: bool) -> Report {
    let mut run = target
        .run("setters", dir)
        .arg(TARGET.to_string())
        .arg(if nulls { "nulls" } else { "nonulls" });
    for (name, value) in env {
        run = run.env(name, value);
    }
    let report = run.run();
    // The two distances the assertions below use are the program's own, so neither side can drift.
    assert_eq!(report.nanos("adjust_s"), ADJUST_S);
    assert_eq!(report.nanos("last_s"), LAST_S);
    assert_eq!(report.nanos("target"), TARGET);
    report
}

/// Whether the run was allowed to set the clock, which the target's own privilege probe answers.
fn allowed(report: &Report) -> bool {
    report.nanos("probe.result") == 0
}

/// Asserts that a call was refused with `errno`.
fn assert_refused(report: &Report, tag: &str, errno: libc::c_int) {
    assert_eq!(
        report.nanos(&format!("{tag}.result")),
        -1,
        "{tag} succeeded"
    );
    assert_eq!(
        report.nanos(&format!("{tag}.errno")),
        i64::from(errno),
        "{tag} was refused with the wrong errno"
    );
}

fn assert_succeeded(report: &Report, tag: &str) {
    assert_eq!(
        report.nanos(&format!("{tag}.result")),
        0,
        "{tag} was refused with errno {}",
        report.get(&format!("{tag}.errno"))
    );
}

fn assert_near(what: &str, value: i64, wanted: i64) {
    assert!(
        (value - wanted).abs() <= SLACK,
        "{what} = {value} is not {wanted} (off by {} s)",
        (value - wanted) / SECOND,
    );
}

/// Asserts that the machine's own clock is where it was, which is what proves a setter never
/// reached the genuine function.
fn assert_the_machine_is_untouched(report: &Report) {
    assert!(
        report.after - report.before < 60 * SECOND,
        "the genuine clock moved by {} s across the run",
        (report.after - report.before) / SECOND
    );
    assert!(
        (report.after - TARGET).abs() > 365 * DAY,
        "the genuine clock is sitting where the target asked us to put the faked one"
    );
}

/// Whether the kernel asks for privilege before it looks at the value a set carries. Linux looks
/// at the value first; macOS and FreeBSD ask first, so a process that may not set the clock hears
/// `EPERM` for a malformed value.
const PRIVILEGE_BEFORE_VALUE: bool = !cfg!(target_os = "linux");

/// Whether it asks before it looks at the clock id too, which FreeBSD alone does: macOS's libc
/// refuses a clock it cannot set before it calls the kernel at all.
const PRIVILEGE_BEFORE_CLOCK: bool = cfg!(target_os = "freebsd");

/// What a malformed call is refused with in a run that may or may not set the clock: `EINVAL`, or
/// `EPERM` where the kernel asks for privilege before it finds what is wrong.
const fn malformed(privilege_first: bool, privileged: bool) -> libc::c_int {
    if privilege_first && !privileged {
        libc::EPERM
    } else {
        libc::EINVAL
    }
}

/// Every malformed call, with the errno it is refused with, which depends on the order the kernel
/// checks a set in and, through that, on whether this run may set the clock.
fn assert_the_malformed_calls_are_refused(report: &Report) {
    let privileged = allowed(report);
    // A clock the kernel keeps but will not set, and one it has never heard of. A platform with no
    // TAI clock has no such id to name, which the target says rather than passing quietly.
    for tag in settable_refusals(report) {
        assert_refused(report, tag, malformed(PRIVILEGE_BEFORE_CLOCK, privileged));
    }
    // A field outside what the call may carry: a fraction wider than a second, a date before the
    // epoch, and a delta wider than libc's own `adjtime` takes.
    for tag in ["bad_nanos", "before_epoch", "bad_micros", "bad_delta"] {
        assert_refused(report, tag, malformed(PRIVILEGE_BEFORE_VALUE, privileged));
    }
    // A pointer the call cannot read, and the question a null delta asks. Linux and macOS look at
    // the clock before the pointer and FreeBSD reads the pointer first; macOS asks for privilege
    // before it reads one. A null delta needs no privilege where it is a question, on Linux and
    // FreeBSD: an adjustment there lands at once rather than slewed, so there is never one still
    // under way to report. macOS reads the delta, after asking for privilege.
    if report.nanos("nulls.available") == 1 {
        let unread = if cfg!(target_os = "macos") && !privileged {
            libc::EPERM
        } else {
            libc::EFAULT
        };
        assert_refused(report, "null_realtime", unread);
        assert_refused(
            report,
            "null_monotonic",
            if cfg!(target_os = "freebsd") {
                libc::EFAULT
            } else {
                libc::EINVAL
            },
        );
        if cfg!(target_os = "macos") {
            assert_refused(report, "no_delta", unread);
        } else {
            assert_succeeded(report, "no_delta");
        }
    }
}

/// Every clock of the family table the target named that the kernel will not set, by the tag it
/// reports it under.
fn settable_refusals(report: &Report) -> Vec<&'static str> {
    let mut tags = vec!["monotonic", "boottime"];
    for (name, tag) in [("coarse.available", "coarse"), ("tai.available", "tai")] {
        if report.nanos(name) == 1 {
            tags.push(tag);
        }
    }
    tags
}

/// Every clock the target named that no family holds, by the tag it reports it under: an id no
/// kernel has heard of, and where the platform has them, another process's CPU clock and a dynamic
/// clock.
fn unfaked_clocks(report: &Report) -> Vec<&'static str> {
    let mut tags = vec!["bogus"];
    for tag in ["other_cpu", "dynamic"] {
        if report.nanos(&format!("{tag}.available")) == 1 {
            tags.push(tag);
        }
    }
    tags
}

/// What a call answered: its result and its errno.
fn answer(report: &Report, tag: &str) -> (i64, i64) {
    (
        report.nanos(&format!("{tag}.result")),
        report.nanos(&format!("{tag}.errno")),
    )
}

/// Asserts that every set of a clock no family holds got the kernel's own answer, which the target
/// asked for a second time through `syscall()`, past libc and the library. macOS has no such call,
/// and its libc refuses any clock but the wall clock before the kernel sees the set.
///
/// The kernel lets nobody set another process's CPU clock: Linux finds the clock and then answers
/// `EPERM`, whatever the caller holds.
fn assert_passed_through(report: &Report) {
    for tag in unfaked_clocks(report) {
        if report.nanos("kernel.available") == 1 {
            assert_eq!(
                answer(report, tag),
                answer(report, &format!("{tag}.kernel")),
                "{tag} was not answered the way the kernel answers it"
            );
        } else {
            assert_refused(report, tag, libc::EINVAL);
        }
    }
    if cfg!(target_os = "linux") && report.nanos("other_cpu.available") == 1 {
        assert_refused(report, "other_cpu", libc::EPERM);
    }
}

#[test]
fn a_clock_the_kernel_cannot_set_is_refused_the_way_the_kernel_refuses_it() {
    for target in every_target() {
        let dir = SessionDir::for_target(target);
        let report = setters(target, &dir, &[]);

        assert_eq!(allowed(&report), target.privileged, "{target:?}");
        assert_the_malformed_calls_are_refused(&report);
    }
}

#[test]
fn a_set_of_a_clock_no_family_holds_reaches_the_kernel() {
    for target in every_target() {
        let dir = SessionDir::for_target(target);
        let report = setters(target, &dir, &[]);

        assert_passed_through(&report);
        assert_the_machine_is_untouched(&report);
    }
}

#[test]
fn an_excluded_command_reaches_the_genuine_setters() {
    let Some(target) = unprivileged_or_skip("the case that lets a setter through to the kernel")
    else {
        return;
    };
    let dir = SessionDir::for_target(target);
    // A command on the skip list never touches a session, so every setter it makes is the
    // platform's own and answers exactly what it would have without us. The commands that confine
    // a target are on it too, so none of them makes a session for the target to join.
    let report = run_setters(
        target,
        &dir,
        &[("LIBTIMEWARP_SKIP_CMDS", "setters,curl,unshare,jail,env")],
        GENUINE_NULLS,
    );

    assert!(
        harness::session_files(dir.path()).is_empty(),
        "an excluded command attached to something"
    );
    assert_the_malformed_calls_are_refused(&report);
    assert_passed_through(&report);
    for tag in ["clock_settime", "adjtime", "settimeofday", "probe"] {
        assert_refused(&report, tag, libc::EPERM);
    }
    assert_the_machine_is_untouched(&report);
}

#[test]
fn a_setter_succeeds_exactly_where_the_kernel_would_have_let_this_process_set_the_clock() {
    for target in every_target() {
        let dir = SessionDir::for_target(target);
        let report = setters(target, &dir, &[]);

        assert_eq!(
            allowed(&report),
            target.privileged,
            "the target answered a privilege the kernel does not grant it, or refused one it does: \
             {target:?}"
        );
        assert_the_malformed_calls_are_refused(&report);
        assert_the_machine_is_untouched(&report);
        if target.privileged {
            assert_the_clock_followed_every_setter(&report);
        } else {
            assert_refused_and_unmoved(&report);
        }
    }
}

/// The three setters that land for a privileged target, each refused with `EPERM`, and nothing
/// moved: every reading is the genuine clock.
fn assert_refused_and_unmoved(report: &Report) {
    for tag in ["probe", "clock_settime", "adjtime", "settimeofday"] {
        assert_refused(report, tag, libc::EPERM);
    }
    for key in [
        "before",
        "after_clock_settime",
        "after_adjtime",
        "after_settimeofday",
    ] {
        assert_near(key, report.nanos(key), report.before);
    }
}

#[test]
fn a_target_with_the_capability_moves_the_clock_the_way_it_asked() {
    let Some(target) = privileged_or_fall_back("the setters that land") else {
        return;
    };
    let dir = SessionDir::for_target(target);
    let report = setters(target, &dir, &[]);

    assert_eq!(allowed(&report), target.privileged, "{target:?}");
    assert_the_machine_is_untouched(&report);
    if target.privileged {
        assert_the_clock_followed_every_setter(&report);
    } else {
        assert_refused_and_unmoved(&report);
    }
}

#[test]
fn root_of_a_user_namespace_or_a_jail_of_its_own_may_not_set_the_clock() {
    let target = match confined() {
        Ok(target) => target,
        Err(why) if cfg!(target_os = "macos") => {
            eprintln!("{why}, so the case that confines a target is not run");
            return;
        }
        Err(why) => {
            skip(why, "the case that confines a target");
            return;
        }
    };
    let dir = SessionDir::for_target(target);
    let report = setters(target, &dir, &[]);

    // Every capability inside a user namespace, or root inside a jail, and still no privilege the
    // kernel would let set the clock: the library hears the kernel's answer and refuses.
    assert_refused_and_unmoved(&report);
    assert_the_malformed_calls_are_refused(&report);
    assert_passed_through(&report);
    assert_the_machine_is_untouched(&report);
}

/// The three setters that land, each read back afterwards: an absolute set, a shift by a delta
/// applied at once rather than slewed, and another absolute set.
fn assert_the_clock_followed_every_setter(report: &Report) {
    assert_succeeded(report, "clock_settime");
    assert_near(
        "after_clock_settime",
        report.nanos("after_clock_settime"),
        TARGET,
    );

    assert_succeeded(report, "adjtime");
    assert_near(
        "after_adjtime",
        report.nanos("after_adjtime"),
        TARGET + ADJUST_S * SECOND,
    );
    // Nothing is ever left pending, because the delta lands at once.
    assert_eq!(report.nanos("adjtime.old_sec"), 0);
    assert_eq!(report.nanos("adjtime.old_usec"), 0);

    assert_succeeded(report, "settimeofday");
    assert_near(
        "after_settimeofday",
        report.nanos("after_settimeofday"),
        TARGET + LAST_S * SECOND,
    );
}

#[test]
fn two_processes_in_one_session_see_the_same_move() {
    let Some(target) = privileged_or_fall_back("one process watching another set the clock") else {
        return;
    };
    let dir = SessionDir::for_target(target);
    let report = target
        .run("settime_pair", &dir)
        .arg(TARGET.to_string())
        .run();

    assert_eq!(
        report.nanos("setter.result") == 0,
        target.privileged,
        "{target:?}"
    );
    assert_the_machine_is_untouched(&report);
    // Before the set, the watching process reads the genuine wall clock, whichever way this goes.
    assert_near(
        "observer.before",
        report.nanos("observer.before"),
        report.before,
    );

    if report.nanos("setter.result") == 0 {
        // The move reached the other process on its next read, and the two agree.
        assert_near("observer.after", report.nanos("observer.after"), TARGET);
        assert_near("setter.after", report.nanos("setter.after"), TARGET);
    } else {
        assert_eq!(report.nanos("setter.errno"), i64::from(libc::EPERM));
        assert_near(
            "observer.after",
            report.nanos("observer.after"),
            report.before,
        );
    }
}

#[test]
fn a_session_on_the_null_keeper_answers_as_an_unprivileged_process_would() {
    let Some(target) =
        privileged_or_fall_back("a null session refusing a process that may set the clock")
    else {
        return;
    };
    let dir = SessionDir::for_target(target);
    let report = setters(target, &dir, &[("LIBTIMEWARP_KEEPER", "null")]);

    // EPERM for the clock that can be set, EINVAL for the ones that cannot, whatever this process
    // actually holds: a session that fakes nothing may not move the machine's clock either.
    assert_the_malformed_calls_are_refused(&report);
    for tag in ["clock_settime", "adjtime", "settimeofday", "probe"] {
        assert_refused(&report, tag, libc::EPERM);
    }
    // A clock no family holds reaches the kernel under either keeper.
    assert_passed_through(&report);
    assert_the_machine_is_untouched(&report);
    for key in ["before", "after_clock_settime", "after_settimeofday"] {
        assert_near(key, report.nanos(key), report.before);
    }
}

/// `LIBTIMEWARP_HOOK` for a hook in the suite, with the arena it should be given.
fn hook_value(name: &str) -> String {
    format!("{}@{ARENA}", harness::hook_library(name).display())
}

/// The entries a logging hook left in the arena, each as many words as that hook writes.
fn logged(session: &Session, entry: usize) -> Vec<Vec<i64>> {
    let words = arena_words(session, usize::try_from(ARENA).unwrap() / size_of::<i64>());
    let (count, entries) = words.split_first().expect("the log's own counter");
    entries
        .chunks_exact(entry)
        .take(usize::try_from(*count).unwrap())
        .map(<[i64]>::to_vec)
        .collect()
}

/// Every setter a hook was handed, each entry beginning with the function, the kind, and the clock.
fn setters_seen_by(session: &Session, entry: usize) -> Vec<Vec<i64>> {
    let setters = [Func::Settimeofday, Func::ClockSettime, Func::Adjtime]
        .map(|func| i64::from(func as u32))
        .to_vec();
    logged(session, entry)
        .into_iter()
        .filter(|seen| seen.first().is_some_and(|func| setters.contains(func)))
        .collect()
}

/// The entry a logging hook recorded for `func` naming `clock`. Both halves matter: one program
/// sets several clocks, so the function alone names more than one call.
fn entry_for(seen: &[Vec<i64>], func: Func, clock: libc::clockid_t) -> &[i64] {
    seen.iter()
        .map(Vec::as_slice)
        .find(|entry| {
            entry.first() == Some(&i64::from(func as u32))
                && entry.get(2) == Some(&i64::from(clock))
        })
        .unwrap_or_else(|| panic!("{func:?} on clock {clock} never reached the hook"))
}

#[test]
fn a_handled_setter_means_success_and_the_keeper_never_moves() {
    let Some(target) = any_target_as_this_user("the case that reads a hook's log") else {
        return;
    };
    let dir = SessionDir::for_target(target);
    let report = setters(
        target,
        &dir,
        &[
            ("LIBTIMEWARP_HOOK", &hook_value("setter_log")),
            // A keeper with a setting of its own, so a value it moved would be recognisable.
            ("LIBTIMEWARP_SHIFT", "-2h"),
        ],
    );

    // The clocks the kernel will not read: on FreeBSD CLOCK_TAI until a time daemon has told the
    // kernel the TAI offset, and of the clocks no family holds, the bogus one and the dynamic one
    // on a descriptor that holds no clock. Another process's CPU clock reads like any other.
    let (unread, read): (Vec<_>, Vec<_>) = settable_refusals(&report)
        .into_iter()
        .partition(|&tag| tag == "tai" && genuine_nanos(clock::TAI).is_none());
    let (unread_unfaked, read_unfaked): (Vec<_>, Vec<_>) = unfaked_clocks(&report)
        .into_iter()
        .partition(|&tag| tag != "other_cpu");

    // Handled, so the program sees success whatever privilege it holds and whichever clock it
    // named. The number the hook wrote into `*out` is ignored, because a setter wants neither.
    for tag in ["clock_settime", "settimeofday", "adjtime"]
        .into_iter()
        .chain(read)
        .chain(read_unfaked.iter().copied())
    {
        assert_succeeded(&report, tag);
    }
    // A clock the kernel will not read never reaches the hook, and neither does a call with no
    // instant to hand it: a malformed one, or a bare probe for privilege. Each is refused as it
    // would be without a hook, and one no family holds the way the kernel refuses it.
    let privileged = target.privileged;
    for tag in unread {
        assert_refused(&report, tag, malformed(PRIVILEGE_BEFORE_CLOCK, privileged));
    }
    for tag in unread_unfaked {
        if report.nanos("kernel.available") == 1 {
            assert_eq!(
                answer(&report, tag),
                answer(&report, &format!("{tag}.kernel"))
            );
        } else {
            assert_refused(&report, tag, libc::EINVAL);
        }
    }
    assert_refused(
        &report,
        "bad_nanos",
        malformed(PRIVILEGE_BEFORE_VALUE, privileged),
    );
    if privileged {
        assert_succeeded(&report, "probe");
    } else {
        assert_refused(&report, "probe", libc::EPERM);
    }

    // The keeper never ran a set, so the wall clock is where the session's own shift put it.
    for key in ["before", "after_clock_settime", "after_settimeofday"] {
        assert_near(key, report.nanos(key), report.before - 2 * 3_600 * SECOND);
    }
    assert_the_machine_is_untouched(&report);

    // Each setter reached the hook in the fake-to-genuine instant shape, carrying the instant the
    // program asked for.
    let session = only_session(dir.path());
    let seen = setters_seen_by(&session, SETTER_ENTRY);
    for entry in &seen {
        assert_eq!(entry[1], i64::from(Kind::InstantToGenuine as u32));
    }
    assert_eq!(
        entry_for(&seen, Func::Settimeofday, clock::REALTIME)[3],
        TARGET + LAST_S * SECOND
    );
    assert_eq!(
        entry_for(&seen, Func::ClockSettime, clock::REALTIME)[3],
        TARGET
    );
    // A clock the keeper would refuse reaches the hook all the same, carrying the id the call
    // named, so a hook that wants it can have it, and so does one no family holds, which the
    // genuine function would have answered had the hook declined it.
    assert_eq!(
        entry_for(&seen, Func::ClockSettime, clock::MONOTONIC)[3],
        TARGET
    );
    if read_unfaked.contains(&"other_cpu") {
        let other = libc::clockid_t::try_from(report.nanos("other_cpu.id")).unwrap();
        assert_eq!(entry_for(&seen, Func::ClockSettime, other)[3], TARGET);
    }
    // A move by a duration names an instant only together with what the family shows now, which is
    // the faked wall clock rather than the genuine one.
    assert_near(
        "the instant adjtime asked for",
        entry_for(&seen, Func::Adjtime, clock::REALTIME)[3],
        report.before - 2 * 3_600 * SECOND + ADJUST_S * SECOND,
    );
}

#[test]
fn a_declined_setter_goes_to_the_keeper_and_never_to_the_genuine_function() {
    let Some(target) =
        privileged_as_this_user_or_fall_back("a declined setter that lands on the keeper")
    else {
        return;
    };
    let dir = SessionDir::for_target(target);
    // A hook that records every call it is handed and declines all of them.
    let report = setters(
        target,
        &dir,
        &[("LIBTIMEWARP_HOOK", &hook_value("clock_group"))],
    );

    // The hook saw all three setters, in the fake-to-genuine instant shape, and declined each one.
    let session = only_session(dir.path());
    let seen = setters_seen_by(&session, DECLINED_ENTRY);
    for func in [Func::Settimeofday, Func::ClockSettime, Func::Adjtime] {
        let entry = entry_for(&seen, func, clock::REALTIME);
        assert_eq!(entry[1], i64::from(Kind::InstantToGenuine as u32));
    }

    // And the keeper answered them, never the genuine function: the machine's own clock is where
    // it was, and a process that may set the clock moved the session instead.
    assert_eq!(allowed(&report), target.privileged, "{target:?}");
    assert_the_machine_is_untouched(&report);
    if target.privileged {
        assert_the_clock_followed_every_setter(&report);
    } else {
        assert_refused_and_unmoved(&report);
    }
}

#[test]
fn a_declined_setter_reaches_the_null_keeper_too() {
    let Some(target) =
        privileged_as_this_user_or_fall_back("a null session refusing a setter the hook declined")
    else {
        return;
    };
    let dir = SessionDir::for_target(target);
    let report = setters(
        target,
        &dir,
        &[
            ("LIBTIMEWARP_KEEPER", "null"),
            ("LIBTIMEWARP_HOOK", &hook_value("clock_group")),
        ],
    );

    let session = only_session(dir.path());
    let seen = setters_seen_by(&session, DECLINED_ENTRY);
    for func in [Func::Settimeofday, Func::ClockSettime, Func::Adjtime] {
        entry_for(&seen, func, clock::REALTIME);
    }

    // The keeper answered each of them as an unprivileged process would see, and the genuine
    // function never ran: the machine's clock is where it was, capability or no capability.
    for tag in ["clock_settime", "adjtime", "settimeofday"] {
        assert_refused(&report, tag, libc::EPERM);
    }
    assert_the_machine_is_untouched(&report);
    assert_near(
        "after_settimeofday",
        report.nanos("after_settimeofday"),
        report.before,
    );
    // A clock no family holds is no keeper's to answer for: declined, its set reaches the kernel.
    assert_passed_through(&report);
}
