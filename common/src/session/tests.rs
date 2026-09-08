use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;

use super::*;
use crate::config::{
    ENV_BOOTTIME_SHIFT, ENV_CPU_RATE, ENV_MONOTONIC_SHIFT, ENV_MONOTONIC_TIMEBASE,
    ENV_REALTIME_RATE, ENV_REALTIME_TIMEBASE, ENV_SHIFT, ENV_TIME, ENV_TIMEKEEP_INTERVAL,
};

fn setup_in(dir: &Path) -> Setup {
    Setup::plain(dir.join("libtimewarp"))
}

/// Genuine readings a plan can be built against without waiting for a clock.
const fn readings(clock: libc::clockid_t) -> Option<Nanos> {
    match clock {
        clock::REALTIME => Some(Nanos(1_700_000_000 * Nanos::SECOND)),
        clock::TAI => Some(Nanos(1_700_000_037 * Nanos::SECOND)),
        clock::MONOTONIC => Some(Nanos(3_600 * Nanos::SECOND)),
        clock::BOOTTIME => Some(Nanos(7_200 * Nanos::SECOND)),
        _ => None,
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
fn creation_leaves_one_whole_session_file_and_no_temporary() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = setup_in(tmp.path());
    let session = create(&setup).unwrap();

    let names: Vec<String> = fs::read_dir(&setup.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].ends_with(".session"), "{names:?}");
    assert!(
        names[0].starts_with(&format!("pid-{}-", std::process::id())),
        "{names:?}"
    );
    assert_eq!(
        session.path().file_name().unwrap().to_string_lossy(),
        names[0]
    );
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
    setup.timekeep_interval = Nanos(2_000_000);
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
    assert_eq!(header.timekeep_interval, 2_000_000);
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
        (&params.monotonic, clock::MONOTONIC),
        (&params.boottime, clock::BOOTTIME),
    ] {
        assert_eq!(line.timebase(), timebase);
        assert_eq!(line.load().rate, Rate::UNITY);
        assert!(line.load().timebase_anchor > Nanos::ZERO);
    }
    assert_eq!(params.cpu_rate.load(Ordering::Relaxed), Rate::UNITY.0);
    // Whole seconds, so exact; a container whose host never set the offset reports 0.
    let tai = platform::genuine_nanos(clock::TAI).unwrap();
    let realtime = platform::genuine_nanos(clock::REALTIME).unwrap();
    let offset = params.tai_offset.load(Ordering::Relaxed);
    assert!((0..=60).contains(&offset), "{offset}");
    assert_eq!(
        offset,
        (tai.0 - realtime.0 + Nanos::SECOND / 2) / Nanos::SECOND
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
    let first = create_named(&setup, "pid-1-fixed").unwrap();
    let before = fs::read(first.path()).unwrap();

    let err = create_named(&setup, "pid-1-fixed").unwrap_err();
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

    let mut bytes = fs::read(&path).unwrap();
    bytes[12..16].copy_from_slice(&(platform::PLATFORM_TAG ^ 0xffff).to_ne_bytes());
    fs::write(&path, bytes).unwrap();

    let err = attach(&path).unwrap_err();
    assert!(
        matches!(err, SessionError::BadSessionFile(BadHeader::Platform)),
        "{err:?}"
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
    let setup = setup_from(&[(ENV_SHIFT, "-2h"), (ENV_TIMEKEEP_INTERVAL, "5ms")]).unwrap();
    assert_eq!(setup.realtime.shift, Nanos(-7_200 * Nanos::SECOND));
    assert_eq!(setup.timekeep_interval, Nanos(5_000_000));
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

#[test]
fn a_timebase_override_is_taken_by_name() {
    let setup = setup_from(&[
        (ENV_REALTIME_TIMEBASE, "CLOCK_BOOTTIME"),
        (ENV_MONOTONIC_TIMEBASE, "CLOCK_MONOTONIC_RAW"),
    ])
    .unwrap();
    assert_eq!(setup.realtime.timebase, clock::BOOTTIME);
    assert_eq!(setup.monotonic.timebase, clock::MONOTONIC_RAW);
    // The one with no override keeps the first clock of its column.
    assert_eq!(setup.boottime.timebase, clock::BOOTTIME);
}

#[test]
fn a_timebase_a_family_may_not_track_is_refused() {
    // `CLOCK_REALTIME` could step, and a step reaching a monotonic family would break it.
    assert_eq!(
        setup_from(&[(ENV_MONOTONIC_TIMEBASE, "CLOCK_REALTIME")]).unwrap_err(),
        BadValue::new(ENV_MONOTONIC_TIMEBASE, Reason::ForbiddenTimebase)
    );
    // Realtime takes `CLOCK_REALTIME` itself but no other clock of its own family.
    assert!(setup_from(&[(ENV_REALTIME_TIMEBASE, "CLOCK_REALTIME")]).is_ok());
    assert_eq!(
        setup_from(&[(ENV_REALTIME_TIMEBASE, "CLOCK_TAI")]).unwrap_err(),
        BadValue::new(ENV_REALTIME_TIMEBASE, Reason::ForbiddenTimebase)
    );
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
    assert_eq!(plan.tai_offset, 37);
    assert_eq!(plan.creation_time, Nanos(1_700_000_000 * Nanos::SECOND));
}

#[test]
fn an_override_changes_where_passage_comes_from_and_leaves_the_zero_alone() {
    let mut setup = setup_in(Path::new("/d"));
    setup.realtime.timebase = clock::BOOTTIME;
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
    setup.boottime.timebase = clock::MONOTONIC;
    let session = create(&setup).unwrap();
    let params = &session.header().live;

    assert_eq!(params.realtime.load().rate, Rate::FROZEN);
    assert_eq!(params.monotonic.load().rate, Rate::from_f64(10.0).unwrap());
    assert_eq!(params.boottime.timebase(), clock::MONOTONIC);
    assert_eq!(params.cpu_rate.load(Ordering::Relaxed), setup.cpu_rate.0);
}

#[test]
fn every_clock_of_a_family_reads_the_family_value() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();
    let header = session.header();

    for group in [
        vec![
            clock::REALTIME,
            clock::REALTIME_COARSE,
            clock::REALTIME_ALARM,
        ],
        vec![
            clock::MONOTONIC,
            clock::MONOTONIC_COARSE,
            clock::MONOTONIC_RAW,
        ],
        vec![clock::BOOTTIME, clock::BOOTTIME_ALARM],
    ] {
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
fn the_clocks_the_family_table_leaves_alone_read_genuine() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();

    // A dynamic clock and an id from `clock_getcpuclockid` are both encoded below zero.
    for id in [-4, -6, 0x7fff_ffff] {
        assert_eq!(clock_now(session.header(), id), None, "{id}");
    }
}

#[test]
fn the_null_keeper_hands_every_clock_back_to_the_genuine_call() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();
    let path = session.path().to_path_buf();
    drop(session);

    let mut bytes = fs::read(&path).unwrap();
    bytes[20..24].copy_from_slice(&(Keeper::Null as u32).to_ne_bytes());
    fs::write(&path, bytes).unwrap();

    let session = attach(&path).unwrap();
    for id in [clock::REALTIME, clock::MONOTONIC, clock::PROCESS_CPUTIME] {
        assert_eq!(clock_now(session.header(), id), None, "{id}");
    }
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
                assert!(clock_now(session.header(), clock::TAI).is_some());
                assert!(clock_now(session.header(), clock::THREAD_CPUTIME).is_some());
            }
        }
    }
}
