use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;

use super::*;
use crate::config::{
    ENV_BOOTTIME_SHIFT, ENV_CPU_RATE, ENV_EMULATE_VDSO, ENV_FAIL_CLOSED, ENV_HOOK, ENV_KEEPER,
    ENV_MONOTONIC_SHIFT, ENV_MONOTONIC_TIMEBASE, ENV_RATE, ENV_REALTIME_RATE,
    ENV_REALTIME_TIMEBASE, ENV_SHIFT, ENV_TIME, family_vars,
};
use layout::{Fault, FaultCode};

fn setup_in(dir: &Path) -> Setup {
    Setup::plain(dir.join("libtimewarp"))
}

/// The clock a family tracks by default here. FreeBSD picks between two columns of the family
/// table by the running kernel's version, so a case that names a family's clock asks for it rather
/// than spelling it.
fn timebase_of(family: Family) -> libc::clockid_t {
    platform::default_timebase(family).unwrap_or(clock::REALTIME)
}

fn monotonic() -> libc::clockid_t {
    timebase_of(Family::Monotonic)
}

fn boottime() -> libc::clockid_t {
    timebase_of(Family::Boottime)
}

/// A clock id no platform has a clock for, so no family holds it.
const STRANGER: libc::clockid_t = 0x7fff_ffff;

/// Every id the family table leaves alone that this platform can spell. Linux encodes dynamic
/// clocks and the ids `clock_getcpuclockid` hands out below zero; macOS spells a `clockid_t`
/// unsigned, so there are no such numbers to ask about there.
#[cfg(target_os = "macos")]
const STRANGERS: &[libc::clockid_t] = &[STRANGER];
/// The same, with the ids a signed `clockid_t` can carry.
#[cfg(not(target_os = "macos"))]
const STRANGERS: &[libc::clockid_t] = &[-4, -6, STRANGER];

/// The whole seconds by which [`readings`] puts TAI ahead of the wall clock.
const TAI_LEAD: i64 = 37;

/// The TAI offset a plan built against [`readings`] carries. macOS keeps no TAI clock, so its TAI
/// id is the wall clock's own, which [`readings`] answers with the wall clock.
const PLANNED_TAI_OFFSET: i64 = if clock::TAI == clock::REALTIME {
    0
} else {
    TAI_LEAD
};

/// Genuine readings a plan can be built against without waiting for a clock.
fn readings(clock: libc::clockid_t) -> Option<Nanos> {
    if clock == clock::REALTIME {
        Some(Nanos(1_700_000_000 * Nanos::SECOND))
    } else if clock == clock::TAI {
        Some(Nanos((1_700_000_000 + TAI_LEAD) * Nanos::SECOND))
    } else if clock == monotonic() {
        Some(Nanos(3_600 * Nanos::SECOND))
    } else if clock == boottime() {
        Some(Nanos(7_200 * Nanos::SECOND))
    } else {
        None
    }
}

fn env_of<'a>(pairs: &'a [(&'static str, &'static str)]) -> impl EnvLookup + use<'a> {
    move |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| std::ffi::OsString::from(*value))
    }
}

fn setup_from(pairs: &[(&'static str, &'static str)]) -> Result<Setup, BadValue> {
    Setup::from_env(&env_of(pairs), PathBuf::from("/d"))
}

#[test]
fn the_directory_is_created_private_to_us() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    ensure_dir(&setup.dir).unwrap();

    let mode = fs::metadata(&setup.dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}

#[test]
fn a_directory_anyone_can_write_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    fs::create_dir(&setup.dir).unwrap();
    fs::set_permissions(&setup.dir, fs::Permissions::from_mode(0o777)).unwrap();

    assert!(matches!(
        ensure_dir(&setup.dir),
        Err(SessionError::NotOurs(_))
    ));
}

#[test]
fn creation_leaves_one_whole_session_file_and_its_lock_and_no_temporary() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.emulate_vdso = false;
    let session = create(&setup).unwrap();

    let mut names: Vec<String> = fs::read_dir(&setup.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let name = session.path().file_name().unwrap().to_string_lossy();
    let stem = name.strip_suffix(naming::SESSION_SUFFIX).unwrap();
    assert!(
        stem.starts_with(&format!("pid-{}-", std::process::id())),
        "{names:?}"
    );
    // The lock file sits beside the session file under the same name, and nothing else is left.
    assert_eq!(names, vec![format!("{stem}.lock"), name.into_owned()]);
}

#[test]
fn a_lock_file_is_private_to_us() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();

    // A wider mode would let another user take a shared lock on it and block every writer for
    // good, and every reader that probes it is its owner by the security checks.
    let lock = naming::lock_path(session.path());
    let mode = fs::metadata(&lock).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{}", lock.display());
}

#[test]
fn the_session_file_is_private_to_us() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();
    let mode = fs::metadata(session.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn every_header_field_is_filled_before_the_file_is_published() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.emulate_vdso = false;
    setup.fail_closed = true;
    let session = create(&setup).unwrap();
    let header = session.header();

    assert_eq!(header.magic, layout::MAGIC);
    assert_eq!(header.abi_version, layout::ABI_VERSION);
    assert_eq!(header.platform, platform::PLATFORM_TAG);
    assert_eq!(header.ready.load(Ordering::Acquire), 1);
    assert_eq!(header.keeper, Keeper::Affine as u32);
    assert_eq!(header.emulate_vdso, 0);
    assert_eq!(header.fail_closed, 1);
    assert_eq!(header.page_size as usize, page_size());
    assert_eq!(header.file_size as usize, layout::file_size(page_size(), 0));
    assert_eq!(header.arena_offset, 0);
    assert_eq!(header.arena_size, 0);
    assert_eq!(header.creator_pid, std::process::id());
    assert!(header.creation_time > 1_700_000_000 * Nanos::SECOND);
    assert_eq!(header.hook_path(), b"");
    assert_eq!(header.fault.load(Ordering::Relaxed), 0);
    assert_eq!(header.seqlock.sequence.load(Ordering::Relaxed) % 2, 0);
    assert_eq!(header.seqlock.writer_state.load(Ordering::Relaxed), 0);
}

#[test]
fn every_family_starts_on_its_own_default_timebase() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();
    let params = &session.header().live;

    for (line, timebase) in [
        (&params.realtime, clock::REALTIME),
        (&params.monotonic, monotonic()),
        (&params.boottime, boottime()),
    ] {
        assert_eq!(line.timebase(), timebase);
        assert_eq!(line.load().rate, Rate::UNITY);
        assert!(line.load().timebase_anchor > Nanos::ZERO);
    }
    assert_eq!(params.cpu_rate.load(Ordering::Relaxed), Rate::UNITY.0);
    // Whole seconds, so exact; a container whose host never set the offset reports 0. A kernel
    // that refuses the clock leaves 0 as well: FreeBSD before 15.0 has none, and 15.0 refuses it
    // until a time daemon has told the kernel the offset.
    let realtime = platform::genuine_nanos(clock::REALTIME).unwrap();
    let offset = params.tai_offset.load(Ordering::Relaxed);
    assert!((0..=60).contains(&offset), "{offset}");
    assert_eq!(
        offset,
        platform::genuine_nanos(clock::TAI)
            .map_or(0, |tai| (tai.0 - realtime.0 + Nanos::SECOND / 2)
                / Nanos::SECOND)
    );
    assert_eq!(params.epoch.load(Ordering::Relaxed), 0);
}

#[test]
fn the_shadow_starts_as_a_whole_copy_of_the_live_parameters() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.realtime.shift = Nanos(-7_200 * Nanos::SECOND);
    let session = create(&setup).unwrap();
    let header = session.header();

    assert_eq!(header.live.realtime.load(), header.shadow.realtime.load());
    assert_eq!(header.live.monotonic.load(), header.shadow.monotonic.load());
    assert_eq!(header.live.boottime.load(), header.shadow.boottime.load());
    assert_eq!(
        header.live.tai_offset.load(Ordering::Relaxed),
        header.shadow.tai_offset.load(Ordering::Relaxed)
    );
}

#[test]
fn a_name_that_collides_fails_instead_of_landing_on_a_live_session() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    let first = create_named(&setup, "pid-1-fixed", None).unwrap();
    let before = fs::read(first.path()).unwrap();

    let err = create_named(&setup, "pid-1-fixed", None).unwrap_err();
    assert!(matches!(err, SessionError::Collision(_)), "{err:?}");

    assert_eq!(
        fs::read(first.path()).unwrap(),
        before,
        "the live session was touched"
    );
    let leftovers: Vec<_> = fs::read_dir(&setup.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(naming::TMP_SUFFIX))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn attaching_applies_the_same_directory_checks_as_creating() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    let session = create(&setup).unwrap();
    let path = session.path().to_path_buf();
    drop(session);

    fs::set_permissions(&setup.dir, fs::Permissions::from_mode(0o777)).unwrap();
    let err = attach(&path).unwrap_err();
    assert!(matches!(err, SessionError::NotOurs(_)), "{err:?}");

    fs::set_permissions(&setup.dir, fs::Permissions::from_mode(0o700)).unwrap();
    attach(&path).unwrap();
}

#[test]
fn a_created_session_attaches_back_to_the_same_parameters() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.realtime.shift = Nanos(-7_200 * Nanos::SECOND);
    let created = create(&setup).unwrap();

    let attached = attach(created.path()).unwrap();
    assert_eq!(attached.header().creator_pid, std::process::id());
    assert_eq!(
        attached.header().live.realtime.load(),
        created.header().live.realtime.load()
    );
}

#[test]
fn attaching_to_nothing_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    ensure_dir(&setup.dir).unwrap();
    let err = attach(&setup.dir.join("absent.session")).unwrap_err();
    assert!(matches!(err, SessionError::NoSession), "{err:?}");
}

#[test]
fn a_foreign_file_is_refused_and_left_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    ensure_dir(&setup.dir).unwrap();
    let path = setup.dir.join("planted.session");
    fs::write(&path, vec![0u8; size_of::<Header>() + 1]).unwrap();

    let err = attach(&path).unwrap_err();
    assert!(
        matches!(err, SessionError::BadSessionFile(BadHeader::Magic)),
        "{err:?}"
    );
    assert!(
        path.exists(),
        "the file we could not read must be left for the user to delete"
    );
}

#[test]
fn a_truncated_file_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    let session = create(&setup).unwrap();
    let path = session.path().to_path_buf();
    drop(session);
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(64).unwrap();

    let err = attach(&path).unwrap_err();
    assert!(
        matches!(err, SessionError::BadSessionFile(BadHeader::Truncated)),
        "{err:?}"
    );
}

#[test]
fn a_file_from_another_abi_version_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    let session = create(&setup).unwrap();
    let path = session.path().to_path_buf();
    drop(session);

    let mut bytes = fs::read(&path).unwrap();
    bytes[8..12].copy_from_slice(&(layout::ABI_VERSION + 1).to_ne_bytes());
    fs::write(&path, bytes).unwrap();

    let err = attach(&path).unwrap_err();
    assert!(
        matches!(err, SessionError::BadSessionFile(BadHeader::AbiVersion)),
        "{err:?}"
    );
}

#[test]
fn a_file_from_another_platform_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    let session = create(&setup).unwrap();
    let path = session.path().to_path_buf();
    drop(session);

    // The tag another supported OS would have written on this architecture. It says which OS's
    // clock ids the parameters carry, so the same numbers name different clocks under it.
    let elsewhere = if platform::OS_TAG == 1 { 2 } else { 1 };
    let mut bytes = fs::read(&path).unwrap();
    bytes[12..16].copy_from_slice(&((elsewhere << 16) | platform::ARCH_TAG).to_ne_bytes());
    fs::write(&path, bytes).unwrap();

    let err = attach(&path).unwrap_err();
    assert!(
        matches!(err, SessionError::BadSessionFile(BadHeader::Platform)),
        "{err:?}"
    );
    assert!(
        path.exists(),
        "the file we could not read must be left for the user to delete"
    );
}

#[test]
fn a_file_whose_ready_flag_never_landed_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    let session = create(&setup).unwrap();
    let path = session.path().to_path_buf();
    drop(session);

    let mut bytes = fs::read(&path).unwrap();
    bytes[16..20].copy_from_slice(&0u32.to_ne_bytes());
    fs::write(&path, bytes).unwrap();

    let err = attach(&path).unwrap_err();
    assert!(
        matches!(err, SessionError::BadSessionFile(BadHeader::NotReady)),
        "{err:?}"
    );
}

#[test]
fn a_planted_symlink_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    let session = create(&setup).unwrap();
    let link = setup.dir.join("planted.session");
    std::os::unix::fs::symlink(session.path(), &link).unwrap();

    let err = attach(&link).unwrap_err();
    assert!(matches!(err, SessionError::Io(_)), "{err:?}");
}

#[test]
fn a_planted_fifo_returns_instead_of_blocking_the_attach() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    ensure_dir(&setup.dir).unwrap();
    let path = setup.dir.join("planted.session");
    let c_path = cstring(&path).unwrap();
    // Safety: `c_path` is a live NUL-terminated path in a directory of ours.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

    // A FIFO with nobody on the other end would block a reader or a writer forever; this call
    // returns because it does neither, and the type check refuses what it opened.
    let err = attach(&path).unwrap_err();
    assert!(matches!(err, SessionError::NotOurs(_)), "{err:?}");
    assert!(path.exists());
}

#[test]
fn a_file_owned_by_another_user_is_refused_and_left_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    let session = create(&setup).unwrap();
    let path = session.path().to_path_buf();
    drop(session);

    let Some(foreign) = chown_to_someone_else(&path) else {
        eprintln!("skipping: no privilege here to plant a foreign-owned file");
        return;
    };
    assert_ne!(foreign, euid());

    let err = attach(&path).unwrap_err();
    assert!(matches!(err, SessionError::NotOurs(_)), "{err:?}");
    assert!(path.exists());
}

/// Chowns `path` to a uid other than ours and leaves it world-writable, the way an adversary's own
/// planted file already is: open enough that the owner check, not the mode, is what refuses it.
/// Needs whatever privilege this process can reach. `None` when it can reach none: a uid mismatch
/// needs a second uid to create, and proving we catch one takes a privilege the check itself does
/// not.
fn chown_to_someone_else(path: &Path) -> Option<u32> {
    let target = u32::from(euid() == 0);
    let sudo = |cmd: &str, arg: &str| {
        std::process::Command::new("sudo")
            .args(["-n", cmd, arg])
            .arg(path)
            .status()
            .is_ok_and(|status| status.success())
    };
    (sudo("chown", &target.to_string()) && sudo("chmod", "666")).then_some(target)
}

#[test]
fn a_file_naming_a_keeper_we_do_not_know_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    let session = create(&setup).unwrap();
    let path = session.path().to_path_buf();
    drop(session);

    let mut bytes = fs::read(&path).unwrap();
    bytes[20..24].copy_from_slice(&99u32.to_ne_bytes());
    fs::write(&path, bytes).unwrap();

    let err = attach(&path).unwrap_err();
    assert!(
        matches!(err, SessionError::BadSessionFile(BadHeader::UnknownKeeper)),
        "{err:?}"
    );
}

#[test]
fn the_realtime_line_is_the_genuine_clock_shifted() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.realtime.shift = Nanos(-7_200 * Nanos::SECOND);
    let session = create(&setup).unwrap();

    let genuine = platform::genuine_nanos(clock::REALTIME).unwrap();
    let faked = clock_now(session.header(), clock::REALTIME).unwrap();
    let error = (faked.0 - (genuine.0 - 7_200 * Nanos::SECOND)).abs();
    assert!(error < Nanos::SECOND, "off by {error} ns");
}

#[test]
fn with_no_shift_the_realtime_line_is_the_genuine_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();

    let genuine = platform::genuine_nanos(clock::REALTIME).unwrap();
    let error = (clock_now(session.header(), clock::REALTIME).unwrap().0 - genuine.0).abs();
    assert!(error < Nanos::SECOND, "off by {error} ns");
}

#[test]
fn setup_reads_the_environment_it_owns() {
    let setup = setup_from(&[(ENV_SHIFT, "-2h")]).unwrap();
    assert_eq!(setup.realtime.shift, Nanos(-7_200 * Nanos::SECOND));
    assert!(setup.emulate_vdso);
    assert!(!setup.fail_closed);
}

#[test]
fn setup_names_the_variable_it_could_not_parse() {
    let bad = setup_from(&[(ENV_SHIFT, "two hours")]).unwrap_err();
    assert_eq!(bad, BadValue::new(ENV_SHIFT, Reason::Parse));
}

#[test]
fn a_bare_shift_moves_realtime_alone() {
    let setup = setup_from(&[(ENV_SHIFT, "-2h")]).unwrap();
    assert_eq!(setup.realtime.shift, Nanos(-7_200 * Nanos::SECOND));
    assert_eq!(setup.monotonic.shift, Nanos::ZERO);
    assert_eq!(setup.boottime.shift, Nanos::ZERO);
}

#[test]
fn a_bare_rate_covers_the_families_that_measure_elapsed_time_and_leaves_cpu_alone() {
    let setup = setup_from(&[("LIBTIMEWARP_RATE", "10")]).unwrap();
    let ten = Rate::from_f64(10.0).unwrap();
    assert_eq!(setup.realtime.rate, ten);
    assert_eq!(setup.monotonic.rate, ten);
    assert_eq!(setup.boottime.rate, ten);
    assert_eq!(setup.cpu_rate, Rate::UNITY);
}

#[test]
fn a_family_variable_wins_over_the_bare_one() {
    let setup = setup_from(&[
        (ENV_SHIFT, "-2h"),
        ("LIBTIMEWARP_REALTIME_SHIFT", "5h"),
        ("LIBTIMEWARP_RATE", "10"),
        (ENV_REALTIME_RATE, "0"),
        (ENV_CPU_RATE, "2"),
    ])
    .unwrap();
    assert_eq!(setup.realtime.shift, Nanos(5 * 3_600 * Nanos::SECOND));
    assert_eq!(setup.realtime.rate, Rate::FROZEN);
    assert_eq!(setup.monotonic.rate, Rate::from_f64(10.0).unwrap());
    assert_eq!(setup.cpu_rate, Rate::from_f64(2.0).unwrap());
}

/// Two clock names this platform's table carries, neither of them a family's default: one for
/// Realtime to track and one for Monotonic, both of them clocks that measure elapsed time.
#[cfg(target_os = "linux")]
const OVERRIDE_NAMES: (&str, &str) = ("CLOCK_BOOTTIME", "CLOCK_MONOTONIC_RAW");
#[cfg(target_os = "freebsd")]
const OVERRIDE_NAMES: (&str, &str) = ("CLOCK_BOOTTIME", "CLOCK_MONOTONIC_PRECISE");
#[cfg(target_os = "macos")]
const OVERRIDE_NAMES: (&str, &str) = ("CLOCK_MONOTONIC_RAW", "CLOCK_UPTIME_RAW_APPROX");

#[test]
fn a_timebase_override_is_taken_by_name() {
    let (for_realtime, for_monotonic) = OVERRIDE_NAMES;
    let setup = setup_from(&[
        (ENV_REALTIME_TIMEBASE, for_realtime),
        (ENV_MONOTONIC_TIMEBASE, for_monotonic),
    ])
    .unwrap();
    assert_eq!(
        setup.realtime.timebase,
        platform::clock_named(for_realtime).unwrap()
    );
    assert_eq!(
        setup.monotonic.timebase,
        platform::clock_named(for_monotonic).unwrap()
    );
    // The one with no override keeps the first clock of its column.
    assert_eq!(setup.boottime.timebase, boottime());
}

#[test]
fn a_timebase_a_family_may_not_track_is_refused() {
    // `CLOCK_REALTIME` could step, and a step reaching a monotonic family would break it.
    assert_eq!(
        setup_from(&[(ENV_MONOTONIC_TIMEBASE, "CLOCK_REALTIME")]).unwrap_err(),
        BadValue::new(ENV_MONOTONIC_TIMEBASE, Reason::ForbiddenTimebase)
    );
    // Realtime takes `CLOCK_REALTIME` itself but no other clock of its own family, of which macOS
    // has none.
    assert!(setup_from(&[(ENV_REALTIME_TIMEBASE, "CLOCK_REALTIME")]).is_ok());
    for &(name, id) in clock::named() {
        if clock::family_of(id) == Some(Family::Realtime) && id != clock::REALTIME {
            assert_eq!(
                setup_from(&[(ENV_REALTIME_TIMEBASE, name)]).unwrap_err(),
                BadValue::new(ENV_REALTIME_TIMEBASE, Reason::ForbiddenTimebase),
                "{name}"
            );
        }
    }
    // As is a Cpu clock, for every family.
    assert_eq!(
        setup_from(&[
            (ENV_BOOTTIME_SHIFT, "0"),
            ("LIBTIMEWARP_BOOTTIME_TIMEBASE", "CLOCK_THREAD_CPUTIME_ID")
        ])
        .unwrap_err(),
        BadValue::new("LIBTIMEWARP_BOOTTIME_TIMEBASE", Reason::ForbiddenTimebase)
    );
}

#[test]
fn a_clock_name_this_platform_does_not_have_is_refused() {
    assert_eq!(
        setup_from(&[(ENV_REALTIME_TIMEBASE, "CLOCK_YESTERDAY")]).unwrap_err(),
        BadValue::new(ENV_REALTIME_TIMEBASE, Reason::UnknownClock)
    );
}

#[test]
fn an_absolute_time_and_a_realtime_shift_say_the_same_thing_twice() {
    assert_eq!(
        setup_from(&[(ENV_TIME, "2020-01-01T00:00:00Z"), (ENV_SHIFT, "-2h")]).unwrap_err(),
        BadValue::new(ENV_TIME, Reason::SaidTwice(ENV_SHIFT))
    );
    assert_eq!(
        setup_from(&[
            (ENV_TIME, "2020-01-01T00:00:00Z"),
            ("LIBTIMEWARP_REALTIME_SHIFT", "-2h")
        ])
        .unwrap_err(),
        BadValue::new(ENV_TIME, Reason::SaidTwice("LIBTIMEWARP_REALTIME_SHIFT"))
    );
    // A shift of another family says something else, so the two stand together.
    assert!(
        setup_from(&[
            (ENV_TIME, "2020-01-01T00:00:00Z"),
            (ENV_MONOTONIC_SHIFT, "1h")
        ])
        .is_ok()
    );
}

#[test]
fn a_plan_anchors_each_family_on_its_default_timebase() {
    let plan = setup_in(Path::new("/d")).plan(&readings).unwrap();
    assert_eq!(
        plan.realtime.fake_anchor,
        Nanos(1_700_000_000 * Nanos::SECOND)
    );
    assert_eq!(plan.realtime.timebase_anchor, plan.realtime.fake_anchor);
    assert_eq!(plan.monotonic.fake_anchor, Nanos(3_600 * Nanos::SECOND));
    assert_eq!(plan.boottime.fake_anchor, Nanos(7_200 * Nanos::SECOND));
    assert_eq!(plan.cpu_rate, Rate::UNITY);
    assert_eq!(plan.tai_offset, PLANNED_TAI_OFFSET);
    assert_eq!(plan.creation_time, Nanos(1_700_000_000 * Nanos::SECOND));
}

#[test]
fn an_override_changes_where_passage_comes_from_and_leaves_the_zero_alone() {
    let mut setup = setup_in(Path::new("/d"));
    setup.realtime.timebase = boottime();
    let plan = setup.plan(&readings).unwrap();

    assert_eq!(
        plan.realtime.fake_anchor,
        Nanos(1_700_000_000 * Nanos::SECOND)
    );
    assert_eq!(plan.realtime.timebase_anchor, Nanos(7_200 * Nanos::SECOND));
}

#[test]
fn an_absolute_time_sets_the_realtime_zero_outright() {
    let mut setup = setup_in(Path::new("/d"));
    setup.realtime_at = Some(Nanos(1_577_836_800 * Nanos::SECOND));
    let plan = setup.plan(&readings).unwrap();

    assert_eq!(
        plan.realtime.fake_anchor,
        Nanos(1_577_836_800 * Nanos::SECOND)
    );
    assert_eq!(
        plan.realtime.timebase_anchor,
        Nanos(1_700_000_000 * Nanos::SECOND)
    );
}

#[test]
fn a_creation_shift_below_zero_is_refused_for_the_nondecreasing_families() {
    fn monotonic(setup: &mut Setup) -> &mut FamilySetup {
        &mut setup.monotonic
    }
    fn boottime(setup: &mut Setup) -> &mut FamilySetup {
        &mut setup.boottime
    }

    type Case = (Family, &'static str, fn(&mut Setup) -> &mut FamilySetup);
    let cases: [Case; 2] = [
        (Family::Monotonic, ENV_MONOTONIC_SHIFT, monotonic),
        (Family::Boottime, ENV_BOOTTIME_SHIFT, boottime),
    ];
    for (family, variable, setup_of) in cases {
        let mut setup = setup_in(Path::new("/d"));
        setup_of(&mut setup).shift = Nanos(-9_000 * Nanos::SECOND);
        assert_eq!(
            setup.plan(&readings).unwrap_err(),
            BadValue::new(variable, Reason::BelowZero),
            "{family}"
        );

        // A shift the family can absorb is taken.
        setup_of(&mut setup).shift = Nanos(-600 * Nanos::SECOND);
        assert!(setup.plan(&readings).is_ok(), "{family}");
    }
}

#[test]
fn a_realtime_value_outside_the_range_an_instant_holds_is_refused() {
    let mut setup = setup_in(Path::new("/d"));
    setup.realtime.shift = Nanos(i64::MAX);
    assert_eq!(
        setup.plan(&readings).unwrap_err(),
        BadValue::new(ENV_REALTIME_SHIFT, Reason::OutOfRange)
    );
}

#[test]
fn a_timebase_the_kernel_refuses_is_a_bad_value() {
    let mut setup = setup_in(Path::new("/d"));
    setup.monotonic.timebase = 0x7fff_ffff;
    assert_eq!(
        setup.plan(&readings).unwrap_err(),
        BadValue::new(ENV_MONOTONIC_TIMEBASE, Reason::UnreadableClock)
    );
}

#[test]
fn a_creator_that_cannot_set_up_what_it_was_asked_for_leaves_the_directory_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.monotonic.shift = Nanos(-1_000_000 * Nanos::SECOND);

    let err = create(&setup).unwrap_err();
    assert!(matches!(err, SessionError::BadValue(_)), "{err:?}");
    assert_eq!(fs::read_dir(&setup.dir).unwrap().count(), 0);
}

#[test]
fn every_family_reads_the_line_the_setup_asked_for() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.realtime.rate = Rate::FROZEN;
    setup.monotonic.rate = Rate::from_f64(10.0).unwrap();
    setup.cpu_rate = Rate::from_f64(4.0).unwrap();
    setup.boottime.timebase = monotonic();
    let session = create(&setup).unwrap();
    let params = &session.header().live;

    assert_eq!(params.realtime.load().rate, Rate::FROZEN);
    assert_eq!(params.monotonic.load().rate, Rate::from_f64(10.0).unwrap());
    assert_eq!(params.boottime.timebase(), monotonic());
    assert_eq!(params.cpu_rate.load(Ordering::Relaxed), setup.cpu_rate.0);
}

/// Every clock of each family that carries a line, from this platform's own column of the family
/// table. TAI is left out: it reads the Realtime line plus an offset, which the case below is for.
fn family_groups() -> Vec<(Family, Vec<libc::clockid_t>)> {
    [Family::Realtime, Family::Monotonic, Family::Boottime]
        .iter()
        .map(|&family| {
            let group = clock::named()
                .iter()
                .map(|&(_, id)| id)
                .filter(|&id| clock::family_of(id) == Some(family) && !clock::is_tai(id))
                .collect();
            (family, group)
        })
        .collect()
}

#[test]
fn every_clock_of_a_family_reads_the_family_value() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();
    let header = session.header();

    for (family, group) in family_groups() {
        // macOS keeps one wall clock, so its Realtime family is that clock alone.
        let alone = cfg!(target_os = "macos") && family == Family::Realtime;
        let least = if alone { 1 } else { 2 };
        assert!(group.len() >= least, "{family} holds only {group:?}");
        let readings: Vec<i64> = group
            .iter()
            .map(|&id| clock_now(header, id).unwrap().0)
            .collect();
        let spread = readings.iter().max().unwrap() - readings.iter().min().unwrap();
        assert!(spread < Nanos::SECOND, "{group:?} spread by {spread} ns");
    }
}

#[test]
fn the_tai_clock_reads_the_realtime_line_plus_the_offset() {
    if clock::family_of(clock::TAI).is_none() {
        // FreeBSD grew a TAI clock at 15.0, so a kernel below that has none to read.
        eprintln!("this kernel has no TAI clock, so that case went unexercised");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.realtime_at = Some(Nanos(631_152_000 * Nanos::SECOND));
    let session = create(&setup).unwrap();
    let header = session.header();

    let offset = header.live.tai_offset.load(Ordering::Relaxed);
    let realtime = clock_now(header, clock::REALTIME).unwrap();
    let tai = clock_now(header, clock::TAI).unwrap();
    // Today's offset whatever era Realtime is set to, and the era is 1990 here.
    assert!(realtime.seconds() < 700_000_000, "{realtime:?}");
    assert!((tai.0 - realtime.0 - offset * Nanos::SECOND).abs() < Nanos::SECOND);
}

#[test]
fn the_cpu_family_scales_each_reading_on_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.cpu_rate = Rate::from_f64(8.0).unwrap();
    let session = create(&setup).unwrap();

    for id in [clock::PROCESS_CPUTIME, clock::THREAD_CPUTIME] {
        let genuine = platform::genuine_nanos(id).unwrap();
        let faked = clock_now(session.header(), id).unwrap();
        // The genuine reading is taken first, so the fake one is at or past eight times it.
        assert!(faked.0 >= 8 * genuine.0, "{faked:?} against {genuine:?}");
        assert!(faked.0 < 8 * genuine.0 + Nanos::SECOND, "{faked:?}");
    }
}

#[test]
fn an_amount_of_consumed_cpu_scales_by_the_cpu_rate() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.cpu_rate = Rate::from_f64(8.0).unwrap();
    let session = create(&setup).unwrap();
    let header = session.header();

    for id in [clock::PROCESS_CPUTIME, clock::THREAD_CPUTIME] {
        assert_eq!(cpu_scaled(header, id, Nanos(250)), Some(Nanos(2_000)));
        assert_eq!(cpu_scaled(header, id, Nanos::ZERO), Some(Nanos::ZERO));
    }
    // A clock of any other family has no such amount to convert, and neither has one we leave
    // alone: the reading of those comes from the clock itself.
    for id in [clock::REALTIME, monotonic(), boottime(), STRANGER] {
        assert_eq!(cpu_scaled(header, id, Nanos(250)), None, "{id}");
    }
}

#[test]
fn a_null_session_leaves_an_amount_of_consumed_cpu_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.keeper = Keeper::Null;
    let session = create(&setup).unwrap();

    assert_eq!(
        cpu_scaled(session.header(), clock::PROCESS_CPUTIME, Nanos(250)),
        None
    );
}

#[test]
fn a_timer_query_converts_the_genuine_remaining_into_the_familys_own_units() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.monotonic.rate = Rate::from_f64(10.0).unwrap();
    setup.cpu_rate = Rate::from_f64(0.5).unwrap();
    let session = create(&setup).unwrap();
    let header = session.header();

    // A tenth of a genuine second left on a ten-times clock is a whole second in the program's own
    // units, which is what it armed the timer for.
    let tenth = Nanos(Nanos::SECOND / 10);
    assert_eq!(
        timer_remaining(header, monotonic(), tenth),
        Some(Nanos(Nanos::SECOND))
    );
    // The Realtime family is left at real speed here, so its timers count down as they were armed.
    assert_eq!(timer_remaining(header, clock::REALTIME, tenth), Some(tenth),);
    // And a Cpu timer follows the Cpu rate, which is the one setting that family has.
    assert_eq!(
        timer_remaining(header, clock::PROCESS_CPUTIME, tenth),
        Some(Nanos(Nanos::SECOND / 20))
    );
    // A clock the family table leaves alone counts down in genuine units.
    assert_eq!(timer_remaining(header, STRANGER, tenth), None);
}

#[test]
fn a_disarmed_timer_has_nothing_left_and_an_armed_one_never_has_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.monotonic.rate = Rate::FROZEN;
    setup.realtime.rate = Rate(1);
    let session = create(&setup).unwrap();
    let header = session.header();

    // Zero is what a disarmed timer reports, on any rate at all, so it converts to itself rather
    // than to the duration a frozen family owes.
    for clock in [monotonic(), clock::REALTIME] {
        assert_eq!(
            timer_remaining(header, clock, Nanos::ZERO),
            Some(Nanos::ZERO)
        );
    }
    // A frozen family's clock never advances, so a timer still armed under it is owed every
    // nanosecond we can name.
    assert_eq!(
        timer_remaining(header, monotonic(), Nanos(1)),
        Some(Nanos(i64::MAX))
    );
    // And the slowest rate there is leaves a timer with the nanosecond that says it is armed,
    // rather than the nothing that would say it is not.
    assert_eq!(
        timer_remaining(header, clock::REALTIME, Nanos(1)),
        Some(Nanos(1))
    );
}

#[test]
fn a_file_timestamp_is_carried_forward_from_the_two_nows() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.realtime.shift = Nanos(-7_200 * Nanos::SECOND);
    let session = create(&setup).unwrap();
    let header = session.header();

    // The clock runs at real speed two hours back, so a file written now shows two hours back and
    // one written an hour ago shows three.
    let genuine = platform::genuine_nanos(clock::REALTIME).unwrap();
    let shown = timestamp_shown(header, genuine).unwrap();
    assert!((shown.0 - genuine.0 + 7_200 * Nanos::SECOND).abs() < Nanos::SECOND);

    let older = genuine.saturating_sub(Nanos(3_600 * Nanos::SECOND));
    let shown = timestamp_shown(header, older).unwrap();
    assert!((shown.0 - genuine.0 + 10_800 * Nanos::SECOND).abs() < Nanos::SECOND);
}

#[test]
fn a_faster_clock_stretches_the_distance_back_to_a_timestamp() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.realtime.rate = Rate::from_f64(10.0).unwrap();
    let session = create(&setup).unwrap();
    let header = session.header();

    // A file a genuine minute old is ten of the program's own minutes old.
    let genuine = platform::genuine_nanos(clock::REALTIME).unwrap();
    let minute = Nanos(60 * Nanos::SECOND);
    let now = timestamp_shown(header, genuine).unwrap();
    let older = timestamp_shown(header, genuine.saturating_sub(minute)).unwrap();
    assert!(
        (now.0 - older.0 - 600 * Nanos::SECOND).abs() < Nanos::SECOND,
        "{now:?} against {older:?}"
    );
}

#[test]
fn a_timestamp_never_reads_before_the_epoch() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.realtime_at = Some(Nanos(Nanos::SECOND));
    let session = create(&setup).unwrap();
    let header = session.header();

    // The wall clock is a second past the epoch, so a file older than that second is older than
    // the epoch in the clock's own terms. It reads at zero and not below it.
    let genuine = platform::genuine_nanos(clock::REALTIME).unwrap();
    for age in [2, 3_600, 86_400] {
        let older = genuine.saturating_sub(Nanos(age * Nanos::SECOND));
        assert_eq!(timestamp_shown(header, older), Some(Nanos::ZERO), "{age} s");
    }
    // And a timestamp the filesystem wrote at the epoch itself, which is older still.
    assert_eq!(timestamp_shown(header, Nanos::ZERO), Some(Nanos::ZERO));
}

#[test]
fn a_set_timestamp_is_measured_back_to_a_genuine_instant() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.realtime.shift = Nanos(-7_200 * Nanos::SECOND);
    let session = create(&setup).unwrap();
    let header = session.header();

    // The program asks for the instant its own clock shows now, which is the genuine now.
    let genuine = platform::genuine_nanos(clock::REALTIME).unwrap();
    let asked = genuine.saturating_sub(Nanos(7_200 * Nanos::SECOND));
    let stored = timestamp_stored(header, asked).unwrap();
    assert!((stored.0 - genuine.0).abs() < Nanos::SECOND, "{stored:?}");

    // And what it stores reads back as what it asked for, which is the whole point of the pair.
    let shown = timestamp_shown(header, stored).unwrap();
    assert!((shown.0 - asked.0).abs() < Nanos::SECOND, "{shown:?}");
}

#[test]
fn a_frozen_clock_stores_the_genuine_now() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.realtime.rate = Rate::FROZEN;
    let session = create(&setup).unwrap();
    let header = session.header();

    // A clock that never advances puts every instant the program could name infinitely far away,
    // so there is no distance to measure and the genuine now is what lands on the file.
    let genuine = platform::genuine_nanos(clock::REALTIME).unwrap();
    for asked in [Nanos::ZERO, genuine, Nanos(i64::MAX)] {
        let stored = timestamp_stored(header, asked).unwrap();
        assert!((stored.0 - genuine.0).abs() < Nanos::SECOND, "{stored:?}");
    }
}

#[test]
fn only_the_realtime_family_reaches_a_file_timestamp() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.monotonic.rate = Rate::from_f64(10.0).unwrap();
    setup.boottime.rate = Rate::FROZEN;
    setup.cpu_rate = Rate::from_f64(8.0).unwrap();
    let session = create(&setup).unwrap();
    let header = session.header();

    // Every other family is moved and the wall clock is not, so a timestamp is left where the
    // filesystem wrote it.
    let genuine = platform::genuine_nanos(clock::REALTIME).unwrap();
    for asked in [Nanos::ZERO, genuine.saturating_sub(Nanos(Nanos::SECOND))] {
        let shown = timestamp_shown(header, asked).unwrap();
        assert!((shown.0 - asked.0).abs() < Nanos::SECOND, "{shown:?}");
        let stored = timestamp_stored(header, asked).unwrap();
        assert!((stored.0 - asked.0).abs() < Nanos::SECOND, "{stored:?}");
    }
}

#[test]
fn a_null_session_leaves_a_file_timestamp_as_the_filesystem_wrote_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.keeper = Keeper::Null;
    let session = create(&setup).unwrap();
    let header = session.header();

    assert_eq!(timestamp_shown(header, Nanos(250)), None);
    assert_eq!(timestamp_stored(header, Nanos(250)), None);
}

#[test]
fn a_null_session_leaves_a_timers_remaining_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.keeper = Keeper::Null;
    let session = create(&setup).unwrap();

    assert_eq!(
        timer_remaining(session.header(), monotonic(), Nanos(250)),
        None
    );
}

#[test]
fn the_clocks_the_family_table_leaves_alone_read_genuine() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();

    for &id in STRANGERS {
        assert_eq!(clock_now(session.header(), id), None, "{id}");
    }
}

#[test]
fn the_null_keeper_hands_every_clock_back_to_the_genuine_call() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.keeper = Keeper::Null;
    let session = create(&setup).unwrap();
    let header = session.header();

    for id in [clock::REALTIME, monotonic(), clock::PROCESS_CPUTIME] {
        assert_eq!(clock_now(header, id), None, "{id}");
    }
    // No seqlock read and no write section: the words the write flow uses are as the creator left
    // them.
    assert_eq!(header.seqlock.sequence.load(Ordering::Relaxed), 0);
    assert_eq!(header.seqlock.writer_state.load(Ordering::Relaxed), 0);
}

#[test]
fn a_null_session_publishes_the_genuine_parameters_it_never_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.keeper = Keeper::Null;
    let session = create(&setup).unwrap();
    let header = session.header();

    assert_eq!(header.keeper, Keeper::Null as u32);
    assert!(header.creation_time > 1_700_000_000 * Nanos::SECOND);
    for (line, timebase) in [
        (&header.live.realtime, clock::REALTIME),
        (&header.live.monotonic, monotonic()),
        (&header.live.boottime, boottime()),
    ] {
        assert_eq!(line.load().rate, Rate::UNITY);
        assert_eq!(line.load().fake_anchor, line.load().timebase_anchor);
        assert_eq!(line.timebase(), timebase);
    }
    // Including the offset a controller reads TAI's lead from, which says what the kernel says.
    let affine = create(&setup_in(tmp.path())).unwrap();
    assert_eq!(
        header.live.tai_offset.load(Ordering::Relaxed),
        affine.header().live.tai_offset.load(Ordering::Relaxed)
    );
}

#[test]
fn a_genuine_plan_refuses_no_reading() {
    let plan = genuine_plan(&readings);
    assert_eq!(
        plan.realtime.fake_anchor,
        readings(clock::REALTIME).unwrap()
    );
    assert_eq!(plan.realtime.timebase_anchor, plan.realtime.fake_anchor);
    assert_eq!(plan.realtime.rate, Rate::UNITY);
    assert_eq!(plan.creation_time, readings(clock::REALTIME).unwrap());
    assert_eq!(plan.tai_offset, PLANNED_TAI_OFFSET);

    // A clock the kernel will not give up lands as zero rather than as a bad value, so a creator
    // recording a fault still gets its session onto disk.
    let refused = |_: libc::clockid_t| None;
    assert_eq!(genuine_plan(&refused).creation_time, Nanos::ZERO);
    assert_eq!(genuine_plan(&refused).tai_offset, 0);
}

#[test]
fn the_null_keeper_reads_none_of_the_clock_settings() {
    // Every one of these is a bad value for the affine keeper, and none of them is read here.
    let setup = setup_from(&[
        (ENV_KEEPER, "null"),
        (ENV_RATE, "-1"),
        (ENV_TIME, "yesterday"),
        (ENV_MONOTONIC_TIMEBASE, "CLOCK_YESTERDAY"),
    ])
    .unwrap();

    assert_eq!(setup.keeper, Keeper::Null);
    assert_eq!(setup.realtime.rate, Rate::UNITY);
}

#[test]
fn a_timekeeper_this_library_does_not_have_is_a_bad_value() {
    assert_eq!(
        setup_from(&[(ENV_KEEPER, "genuine")]).unwrap_err(),
        BadValue::new(ENV_KEEPER, Reason::UnknownKeeper)
    );
    assert_eq!(
        setup_from(&[(ENV_KEEPER, "affine")]).unwrap().keeper,
        Keeper::Affine
    );
    // The keeper is the session's, and the default is the affine one.
    assert_eq!(setup_from(&[]).unwrap().keeper, Keeper::Affine);
}

#[test]
fn a_creator_that_fell_back_publishes_a_null_session_carrying_the_fault() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env_of(&[(ENV_FAIL_CLOSED, "1"), (ENV_EMULATE_VDSO, "0")]);
    let setup = Setup::fallback(&env, tmp.path().join("libtimewarp"));
    let fault = Fault::new(FaultCode::BadValue, std::process::id());

    let session = create_faulted(&setup, fault).unwrap();
    let header = session.header();

    assert_eq!(header.keeper, Keeper::Null as u32);
    assert_eq!(header.hook_path(), b"", "a fault leaves no hook either");
    assert_eq!(header.fault(), Some(fault));
    // The flags the session owns are still the environment's to say.
    assert_eq!(header.fail_closed, 1);
    assert_eq!(header.emulate_vdso, 0);
    // And the file is whole, beside its lock file alone, so a child attaches to it as it would to
    // any other.
    assert_eq!(fs::read_dir(&setup.dir).unwrap().count(), 2);
    assert_eq!(
        attach(session.path()).unwrap().header().fault(),
        Some(fault)
    );
}

#[test]
fn the_first_fault_holds_the_slot_for_the_session() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();
    let header = session.header();
    assert_eq!(header.fault(), None, "a healthy session carries none");

    let first = Fault::new(FaultCode::BadValue, 11);
    assert!(header.record_fault(first));
    assert!(!header.record_fault(Fault::new(FaultCode::BadValue, 22)));
    assert_eq!(header.fault(), Some(first));
}

#[test]
fn the_session_says_whether_a_process_fails_closed_and_the_environment_says_when_none_does() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.fail_closed = true;
    let session = create(&setup).unwrap();

    // An attacher reads none of the setup variables, so the header wins over the environment.
    let says_no = env_of(&[(ENV_FAIL_CLOSED, "0")]);
    assert!(fails_closed(Some(session.header()), &says_no));
    // With no session to follow, the variable decides on its own.
    assert!(!fails_closed(None, &says_no));
    assert!(fails_closed(None, &env_of(&[(ENV_FAIL_CLOSED, "1")])));
    assert!(!fails_closed(None, &env_of(&[])));
}

#[test]
fn the_keeper_computation_survives_garbage_parameters() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();
    let params = &session.header().live;

    // A torn read's value is discarded rather than prevented, so every bit pattern has to compute.
    for &fake_anchor in &[i64::MIN, -1, 0, i64::MAX] {
        for &timebase_anchor in &[i64::MIN, 0, i64::MAX] {
            for &rate in &[0, u64::MAX, 1] {
                params.realtime.store(
                    Line {
                        fake_anchor: Nanos(fake_anchor),
                        timebase_anchor: Nanos(timebase_anchor),
                        rate: Rate(rate),
                    },
                    clock::REALTIME,
                );
                params.tai_offset.store(i64::MIN, Ordering::Relaxed);
                params.cpu_rate.store(u64::MAX, Ordering::Relaxed);
                assert!(clock_now(session.header(), clock::REALTIME).is_some());
                // Where a family holds a TAI clock: FreeBSD before 15.0 has none.
                assert_eq!(
                    clock_now(session.header(), clock::TAI).is_some(),
                    clock::family_of(clock::TAI).is_some()
                );
                assert!(clock_now(session.header(), clock::THREAD_CPUTIME).is_some());
            }
        }
    }
}

#[test]
fn a_hook_is_read_whichever_keeper_the_session_runs() {
    let library = "/nonexistent/lib.so";
    for keeper in [Keeper::Affine, Keeper::Null] {
        let setup = setup_from(&[
            (ENV_KEEPER, keeper.as_str()),
            (ENV_HOOK, "/nonexistent/lib.so@4096"),
        ])
        .unwrap();
        let hook = setup.hook.expect("the hook is read on either keeper");
        assert_eq!(hook.path, PathBuf::from(library));
        assert_eq!(hook.arena_size, 4096);
    }
    // And an empty value is unset, like every other variable.
    assert!(setup_from(&[(ENV_HOOK, "")]).unwrap().hook.is_none());
    assert!(setup_from(&[]).unwrap().hook.is_none());
}

#[test]
fn a_session_with_no_hook_records_neither_a_path_nor_an_arena() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();
    let header = session.header();

    assert_eq!(header.hook_path(), b"");
    assert_eq!((header.arena_offset, header.arena_size), (0, 0));
    assert!(session.hook().is_none());
    // Nothing to load, so joining the session is no work at all.
    let mut joined = attach(session.path()).unwrap();
    assert_eq!(joined.load_hook(), Ok(()));
    assert!(joined.hook().is_none());
}

#[test]
fn a_hook_the_loader_refuses_leaves_the_directory_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.hook = Spec::parse(OsStr::new("/nonexistent/lib.so@4096"));

    let err = create(&setup).unwrap_err();
    match err {
        SessionError::BadValue(bad) => assert_eq!(
            bad,
            BadValue::new(ENV_HOOK, Reason::Hook(hook::HookError::Unloadable))
        ),
        other => panic!("{other:?}"),
    }
    // Not the session file, and not the lock file either: nothing for the user to clean up.
    assert_eq!(fs::read_dir(&setup.dir).unwrap().count(), 0);
}

#[test]
fn an_arena_no_session_file_could_carry_is_refused_before_anything_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let mut setup = setup_in(tmp.path());
    setup.hook = Spec::parse(OsStr::new("/nonexistent/lib.so@4294967296"));

    let err = create(&setup).unwrap_err();
    match err {
        SessionError::BadValue(bad) => assert_eq!(
            bad,
            BadValue::new(ENV_HOOK, Reason::Hook(hook::HookError::Oversized))
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read_dir(&setup.dir).unwrap().count(), 0);
}

#[test]
fn independent_family_rates_are_accepted() {
    // The wall clock and the elapsed clock may run at rates of their own.
    let elapsed = page_elapsed_family().expect("CLOCK_MONOTONIC belongs to a family");
    let separated = [(ENV_REALTIME_RATE, "2"), (family_vars(elapsed).rate, "3")];
    assert!(setup_from(&separated).is_ok());
    // A bare rate covers both, the usual way of asking for a fast clock.
    assert!(setup_from(&[(ENV_RATE, "10")]).is_ok());
}
