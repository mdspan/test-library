use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;

use super::*;
use crate::config::{ENV_SHIFT, ENV_TIMEKEEP_INTERVAL};

fn setup_in(dir: &Path) -> Setup {
    Setup {
        dir: dir.join("libtimewarp"),
        realtime_shift: Nanos::ZERO,
        emulate_vdso: true,
        fail_closed: false,
        timekeep_interval: DEFAULT_TIMEKEEP_INTERVAL,
    }
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
fn every_family_gets_a_line_even_though_only_realtime_is_read_yet() {
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
    setup.realtime_shift = Nanos(-7_200 * Nanos::SECOND);
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
    setup.realtime_shift = Nanos(-7_200 * Nanos::SECOND);
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
    setup.realtime_shift = Nanos(-7_200 * Nanos::SECOND);
    let session = create(&setup).unwrap();

    let genuine = platform::genuine_nanos(clock::REALTIME).unwrap();
    let faked = realtime_now(session.header()).unwrap();
    let error = (faked.0 - (genuine.0 - 7_200 * Nanos::SECOND)).abs();
    assert!(error < Nanos::SECOND, "off by {error} ns");
}

#[test]
fn with_no_shift_the_realtime_line_is_the_genuine_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let session = create(&setup_in(tmp.path())).unwrap();

    let genuine = platform::genuine_nanos(clock::REALTIME).unwrap();
    let error = (realtime_now(session.header()).unwrap().0 - genuine.0).abs();
    assert!(error < Nanos::SECOND, "off by {error} ns");
}

#[test]
fn setup_reads_the_environment_it_owns() {
    let env = |name: &str| match name {
        ENV_SHIFT => Some("-2h".into()),
        ENV_TIMEKEEP_INTERVAL => Some("5ms".into()),
        _ => None,
    };
    let setup = Setup::from_env(&env, PathBuf::from("/d")).unwrap();
    assert_eq!(setup.realtime_shift, Nanos(-7_200 * Nanos::SECOND));
    assert_eq!(setup.timekeep_interval, Nanos(5_000_000));
    assert!(setup.emulate_vdso);
    assert!(!setup.fail_closed);
}

#[test]
fn setup_names_the_variable_it_could_not_parse() {
    let env = |name: &str| (name == ENV_SHIFT).then(|| "two hours".into());
    let bad = Setup::from_env(&env, PathBuf::from("/d")).unwrap_err();
    assert_eq!(bad.variable, ENV_SHIFT);
}
