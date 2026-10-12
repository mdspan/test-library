//! The session file a running process carries the path of in its environment.
//!
//! Each platform hands over the environment as NUL-separated entries: Linux in
//! `/proc/<pid>/environ`, FreeBSD through `kern.proc.env`, macOS as the tail of
//! `kern.procargs2`.

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use common::config::ENV_SHARED_FILE;

use crate::failure::Failure;

/// The session file process `pid` names in `LIBTIMEWARP__SHARED_FILE`.
pub(crate) fn shared_file_of(pid: u32) -> Result<PathBuf, Failure> {
    let environment = environment(pid).map_err(|why| {
        Failure::Other(format!("cannot read the environment of pid {pid}: {why}"))
    })?;
    shared_file(&environment).ok_or_else(|| {
        Failure::Other(format!(
            "pid {pid} carries no {ENV_SHARED_FILE}, so it is in no session of ours"
        ))
    })
}

/// The value of the first `LIBTIMEWARP__SHARED_FILE` entry of NUL-separated `environment`, unless
/// it is empty.
fn shared_file(environment: &[u8]) -> Option<PathBuf> {
    environment
        .split(|&byte| byte == 0)
        .find_map(|entry| {
            entry
                .strip_prefix(ENV_SHARED_FILE.as_bytes())?
                .strip_prefix(b"=")
        })
        .filter(|value| !value.is_empty())
        .map(|value| PathBuf::from(OsStr::from_bytes(value)))
}

#[cfg(target_os = "linux")]
fn environment(pid: u32) -> io::Result<Vec<u8>> {
    std::fs::read(format!("/proc/{pid}/environ"))
}

#[cfg(target_os = "freebsd")]
fn environment(pid: u32) -> io::Result<Vec<u8>> {
    // The environment can grow between sizing the buffer and filling it.
    const ATTEMPTS: usize = 4;
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_ENV,
        process_id(pid)?,
    ];
    let mut attempt = 1;
    loop {
        let mut buf = vec![0; sysctl(&mut mib, None)?];
        match sysctl(&mut mib, Some(&mut buf)) {
            Ok(len) => {
                buf.truncate(len);
                return Ok(buf);
            }
            Err(err) if err.raw_os_error() == Some(libc::ENOMEM) && attempt < ATTEMPTS => {
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(target_os = "macos")]
fn environment(pid: u32) -> io::Result<Vec<u8>> {
    let mut max = [0; size_of::<libc::c_int>()];
    sysctl(&mut [libc::CTL_KERN, libc::KERN_ARGMAX], Some(&mut max))?;
    let max = usize::try_from(libc::c_int::from_ne_bytes(max))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "kern.argmax is negative"))?;
    let mut buf = vec![0; max];
    let len = sysctl(
        &mut [libc::CTL_KERN, libc::KERN_PROCARGS2, process_id(pid)?],
        Some(&mut buf),
    )?;
    buf.truncate(len);
    procargs_environment(&buf)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "kern.procargs2 returned a malformed argument area",
            )
        })
}

#[cfg(any(target_os = "freebsd", target_os = "macos"))]
fn process_id(pid: u32) -> io::Result<libc::pid_t> {
    libc::pid_t::try_from(pid).map_err(|_| io::Error::from_raw_os_error(libc::ESRCH))
}

/// Reads the value `mib` names into `buf`, or with no `buf` asks only its size, and gives the
/// length the kernel reports.
#[cfg(any(target_os = "freebsd", target_os = "macos"))]
fn sysctl(mib: &mut [libc::c_int], buf: Option<&mut [u8]>) -> io::Result<usize> {
    let namelen = libc::c_uint::try_from(mib.len())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let (old, mut len) = buf.map_or((std::ptr::null_mut(), 0), |buf| {
        (buf.as_mut_ptr().cast(), buf.len())
    });
    // SAFETY: `mib` has `namelen` entries, `old` is null or has `len` writable bytes, and the null
    // new value with length 0 sets nothing.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            namelen,
            old,
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if status == 0 {
        Ok(len)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The environment entries of a `kern.procargs2` area, NUL-separated: after a native-endian `int`
/// argc, the executable path, its NUL padding, and argc arguments, the entries up to an empty one
/// or the end. `None` when the area ends before the arguments do.
#[cfg_attr(
    not(any(target_os = "macos", test)),
    expect(dead_code, reason = "only macOS returns this layout")
)]
fn procargs_environment(area: &[u8]) -> Option<&[u8]> {
    let (argc, rest) = area.split_first_chunk::<{ size_of::<libc::c_int>() }>()?;
    let argc = usize::try_from(libc::c_int::from_ne_bytes(*argc)).ok()?;
    let rest = after_string(rest)?;
    let Some(padding) = rest.iter().position(|&byte| byte != 0) else {
        return (argc == 0).then_some(&[]);
    };
    let mut rest = rest.get(padding..)?;
    for _ in 0..argc {
        rest = after_string(rest)?;
    }
    let mut len = 0;
    while let Some(tail) = rest.get(len..).filter(|tail| !tail.is_empty()) {
        match tail.iter().position(|&byte| byte == 0) {
            Some(0) => break,
            Some(end) => len += end + 1,
            None => len = rest.len(),
        }
    }
    rest.get(..len)
}

/// What follows the first NUL-terminated string of `bytes`.
fn after_string(bytes: &[u8]) -> Option<&[u8]> {
    let end = bytes.iter().position(|&byte| byte == 0)?;
    bytes.get(end + 1..)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(argc: libc::c_int, tail: &[u8]) -> Vec<u8> {
        let mut area = argc.to_ne_bytes().to_vec();
        area.extend_from_slice(tail);
        area
    }

    #[test]
    fn the_first_entry_of_the_variable_wins() {
        let environment = b"HOME=/root\0LIBTIMEWARP__SHARED_FILE=/a b/x.session\0\
                            LIBTIMEWARP__SHARED_FILE=/other\0";
        assert_eq!(
            shared_file(environment),
            Some(PathBuf::from("/a b/x.session"))
        );
    }

    #[test]
    fn a_missing_or_empty_variable_is_no_session() {
        assert_eq!(shared_file(b""), None);
        assert_eq!(shared_file(b"LIBTIMEWARP__SHARED_FILEX=/x\0"), None);
        assert_eq!(shared_file(b"XLIBTIMEWARP__SHARED_FILE=/x\0"), None);
        assert_eq!(shared_file(b"LIBTIMEWARP__SHARED_FILE\0"), None);
        assert_eq!(
            shared_file(b"LIBTIMEWARP__SHARED_FILE=\0LIBTIMEWARP__SHARED_FILE=/x\0"),
            None
        );
    }

    #[test]
    fn an_unterminated_last_entry_counts() {
        assert_eq!(
            shared_file(b"A=1\0LIBTIMEWARP__SHARED_FILE=/x"),
            Some(PathBuf::from("/x"))
        );
    }

    #[test]
    fn procargs_skips_the_path_the_padding_and_the_arguments() {
        let area = area(
            2,
            b"/bin/member\0\0\0\0member\0-v\0A=1\0B=2 3\0\0ptr_munge=\0executable_path=/x\0",
        );
        assert_eq!(procargs_environment(&area), Some(&b"A=1\0B=2 3\0"[..]));
    }

    #[test]
    fn procargs_environment_can_run_to_the_end() {
        assert_eq!(
            procargs_environment(&area(1, b"/m\0m\0A=1\0B=2")),
            Some(&b"A=1\0B=2"[..])
        );
        assert_eq!(
            procargs_environment(&area(1, b"/m\0m\0A=1\0")),
            Some(&b"A=1\0"[..])
        );
        assert_eq!(procargs_environment(&area(1, b"/m\0m\0")), Some(&b""[..]));
    }

    #[test]
    fn procargs_with_no_arguments_and_no_environment() {
        assert_eq!(procargs_environment(&area(0, b"/m\0\0\0")), Some(&b""[..]));
        assert_eq!(procargs_environment(&area(0, b"/m\0")), Some(&b""[..]));
        assert_eq!(
            procargs_environment(&area(0, b"/m\0\0A=1\0\0")),
            Some(&b"A=1\0"[..])
        );
    }

    #[test]
    fn a_truncated_procargs_area_is_refused() {
        assert_eq!(procargs_environment(b""), None);
        assert_eq!(procargs_environment(&[1, 0]), None);
        assert_eq!(procargs_environment(&area(1, b"")), None);
        assert_eq!(procargs_environment(&area(1, b"/m")), None);
        assert_eq!(procargs_environment(&area(1, b"/m\0\0\0")), None);
        assert_eq!(procargs_environment(&area(2, b"/m\0m\0")), None);
        assert_eq!(procargs_environment(&area(2, b"/m\0m\0-v")), None);
        assert_eq!(procargs_environment(&area(-1, b"/m\0m\0")), None);
    }

    #[test]
    fn the_variable_is_found_in_a_procargs_area() {
        let area = area(
            1,
            b"/m\0\0m\0LIBTIMEWARP__SHARED_FILE=/a b/x.session\0\0executable_path=/m\0",
        );
        let environment = procargs_environment(&area).expect("a whole area");
        assert_eq!(
            shared_file(environment),
            Some(PathBuf::from("/a b/x.session"))
        );
    }
}
