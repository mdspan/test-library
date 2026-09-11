//! Starting an updater: the double fork, and the two ways the library can be the program.
//!
//! The library is the updater. There is no second binary to ship or to find, and a fork that kept
//! running would be a copy of the target, large, under the target's name and OOM score, with other
//! threads' locks held. So the fork execs.
//!
//! Which program it execs depends on whether this library is one. A shared object linked the
//! ordinary way names no interpreter, and the kernel would load it with nothing to resolve its
//! imports, so the choice is made by looking rather than by trying: an object that names an
//! interpreter is exec'd by its own path and its `main` runs the loop, and anything else is reached
//! by exec'ing a host program with the library preloaded, where the constructor runs the loop
//! because the host's `main` never will.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::config::{ENV_LOG, ENV_LOG_LEVEL, ENV_PRELOAD, ENV_SHARED_FILE, ENV_UPDATER, EnvLookup};

use super::{c_path, library};

/// The argument that says what an updater is for. The session file follows it.
pub const UPDATER_FLAG: &str = "--updater";

/// The host programs the fallback may exec, most fitting first. Whichever is there is exec'd with
/// the library preloaded; it never reaches its own `main`, because the constructor of the library
/// runs the loop first and does not return, so what the program itself would have done is moot.
const HOSTS: [&str; 2] = ["/usr/bin/true", "/bin/sh"];

/// Where the updater's own standard descriptors go.
///
/// It outlives whoever spawned it, so keeping that process's descriptors would hold a pipe open
/// long after the program on the other end had finished reading it.
const NOWHERE: &str = "/dev/null";

/// What the child reports when the exec itself failed, which is what a shell reports for the same.
const EXEC_FAILED: libc::c_int = 127;

/// What a fresh updater is exec'd with: the program, its arguments, and its whole environment.
///
/// Every string is built before the fork, because between a fork and its exec only
/// async-signal-safe work may happen and building one is not.
#[derive(Debug)]
struct Command {
    program: CString,
    args: Vec<CString>,
    env: Vec<CString>,
    nowhere: CString,
}

impl Command {
    /// The command that starts an updater for `session`, or `None` when this process cannot name
    /// the library to run.
    fn build(session: &Path, env: &impl EnvLookup) -> Option<Self> {
        let library = library()?;
        let itself = is_a_program(&library);
        let program = if itself {
            c_path(&library).ok()?
        } else {
            let host = HOSTS
                .into_iter()
                .map(Path::new)
                .find(|host| host.exists())?;
            c_path(host).ok()?
        };

        let args = vec![
            program.clone(),
            CString::new(UPDATER_FLAG).ok()?,
            c_path(session).ok()?,
        ];

        // The session goes into the environment as well as the arguments, because the two entry
        // points reach it differently: `main` has the arguments and a constructor does not.
        let mut settings = vec![
            setting(ENV_UPDATER, OsStr::new("1"))?,
            setting(ENV_SHARED_FILE, session.as_os_str())?,
        ];
        if !itself {
            settings.push(setting(ENV_PRELOAD, library.as_os_str())?);
        }
        // Where this process logs, so an updater's lines land where its session's do. The
        // descriptor form is left behind on purpose: it names a descriptor of the spawner's, and
        // the updater's own three are closed below. Nothing else is carried over, because an
        // updater reads no setup variable: it joins a session that already exists.
        for name in [ENV_LOG, ENV_LOG_LEVEL] {
            if let Some(value) = env.nonempty(name) {
                settings.push(setting(name, &value)?);
            }
        }
        Some(Self {
            program,
            args,
            env: settings,
            nowhere: CString::new(NOWHERE).ok()?,
        })
    }

    /// The two NULL-terminated arrays `execve` takes.
    fn vectors(&self) -> (Vec<*const libc::c_char>, Vec<*const libc::c_char>) {
        let terminated = |strings: &[CString]| {
            strings
                .iter()
                .map(|string| string.as_ptr())
                .chain(std::iter::once(std::ptr::null()))
                .collect::<Vec<_>>()
        };
        (terminated(&self.args), terminated(&self.env))
    }
}

/// One setting, spelled the way an environment carries it.
fn setting(name: &str, value: &OsStr) -> Option<CString> {
    let mut bytes = Vec::with_capacity(name.len() + 1 + value.len());
    bytes.extend_from_slice(name.as_bytes());
    bytes.push(b'=');
    bytes.extend_from_slice(value.as_bytes());
    CString::new(bytes).ok()
}

/// Starts an updater for `session`, by double fork with `setsid`.
///
/// The intermediate is reaped on the spot, so the updater runs as init's child and the process that
/// spawned it never observes a stray child of its own. That matters because the spawner is somebody
/// else's program: a target waiting on its own children must not find one of ours among them.
///
/// # Errors
/// [`io::Error`] when this process cannot name the library to run, or when the fork itself fails.
pub(super) fn updater(session: &Path, env: &impl EnvLookup) -> io::Result<()> {
    let command = Command::build(session, env)
        .ok_or_else(|| io::Error::other("no libtimewarp library to run the updater from"))?;
    let (args, settings) = command.vectors();

    // Safety: `fork` takes nothing. The child below makes only async-signal-safe calls, and every
    // string the exec needs was built above.
    match unsafe { libc::fork() } {
        -1 => Err(io::Error::last_os_error()),
        0 => {
            // A session of its own, so the updater outlives the terminal its spawner was started
            // from and belongs to no process group anybody signals.
            // Safety: `setsid` takes nothing.
            unsafe { libc::setsid() };
            // Safety: as above. The grandchild execs at once, and the intermediate leaves at once,
            // so neither runs a line of the spawner's program.
            match unsafe { libc::fork() } {
                0 => {
                    // Safety: the path is live and NUL-terminated, and this child has run nothing
                    // of the program's that could be using these descriptors.
                    unsafe { detach(command.nowhere.as_ptr()) };
                    // Safety: the program is a live NUL-terminated path and both vectors are live,
                    // NULL-terminated arrays of live NUL-terminated strings.
                    unsafe {
                        libc::execve(command.program.as_ptr(), args.as_ptr(), settings.as_ptr())
                    };
                    // Safety: `_exit` ends this child, which has run nothing of the program's.
                    unsafe { libc::_exit(EXEC_FAILED) }
                }
                // Safety: as above. The intermediate's only job was to be reaped.
                _ => unsafe { libc::_exit(0) },
            }
        }
        intermediate => {
            reap(intermediate);
            Ok(())
        }
    }
}

/// Points the three standard descriptors at `nowhere`, so the updater holds none of the
/// descriptors its spawner was using.
///
/// Without this a program reading the target's output would wait for the updater to die before it
/// saw the end of the stream, because the pipe stays open while anybody holds the writing end.
///
/// Every call here is async-signal-safe, which is all a child between a fork and its exec may make.
///
/// # Safety
/// `nowhere` must be a live NUL-terminated path, and this child must have run nothing of the
/// program's since the fork.
unsafe fn detach(nowhere: *const libc::c_char) {
    // Safety: the caller guarantees the path.
    let null = unsafe { libc::open(nowhere, libc::O_RDWR) };
    if null < 0 {
        return;
    }
    for standard in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        // Safety: `null` is open and the three standard numbers are ours to replace here.
        unsafe { libc::dup2(null, standard) };
    }
    if null > libc::STDERR_FILENO {
        // Safety: we opened this descriptor and the copies above stand on their own.
        unsafe { libc::close(null) };
    }
}

/// Waits for the intermediate, which exits as soon as it has forked.
///
/// An interrupted wait is retried, and a wait for a child somebody else already reaped answers at
/// once, which is what a program with a `SIGCHLD` handler of its own leaves behind.
fn reap(pid: libc::pid_t) {
    let mut status: libc::c_int = 0;
    loop {
        // Safety: `pid` is a child of this process and `status` is a live word.
        if unsafe { libc::waitpid(pid, &raw mut status, 0) } >= 0 {
            return;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
    }
}

/// The first four bytes of every ELF object.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

/// 64-bit objects, the only width either architecture we support has.
const ELF_CLASS_64: u8 = 2;

/// The program header that names the interpreter, `PT_INTERP`.
const PT_INTERP: u32 = 3;

/// How far into the ELF header the program header table's offset, entry size and count sit, and how
/// long the header is. Fixed by the format for a 64-bit object.
const E_PHOFF: usize = 32;
const E_PHENTSIZE: usize = 54;
const E_PHNUM: usize = 56;
const E_HEADER: usize = 64;

/// How many program headers are walked before what is being walked is taken for something other
/// than a table of them. Longer than any object's.
const MAX_PROGRAM_HEADERS: usize = 128;

/// Whether `path` is an object the kernel will run as a program: a 64-bit ELF that names an
/// interpreter.
///
/// This is the question the fallback turns on. A library the loader can preload but the kernel
/// cannot start answers `false`, and the updater is reached by exec'ing a host program instead.
fn is_a_program(path: &Path) -> bool {
    read_interpreter(path).unwrap_or(false)
}

/// The same, with the failures spelled out, so a file that cannot be read is not a program either.
fn read_interpreter(path: &Path) -> Option<bool> {
    let file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; E_HEADER];
    file.read_exact_at(&mut header, 0).ok()?;
    if header.get(..4)? != ELF_MAGIC || header.get(4) != Some(&ELF_CLASS_64) {
        return Some(false);
    }
    let offset = u64::from_ne_bytes(header.get(E_PHOFF..E_PHOFF + 8)?.try_into().ok()?);
    let width = usize::from(u16::from_ne_bytes(
        header.get(E_PHENTSIZE..E_PHENTSIZE + 2)?.try_into().ok()?,
    ));
    let count = usize::from(u16::from_ne_bytes(
        header.get(E_PHNUM..E_PHNUM + 2)?.try_into().ok()?,
    ));
    if width < 4 || count > MAX_PROGRAM_HEADERS {
        return Some(false);
    }

    let mut kind = [0u8; 4];
    for index in 0..count {
        let at = offset.checked_add(u64::try_from(index.checked_mul(width)?).ok()?)?;
        if file.read_exact_at(&mut kind, at).is_err() {
            return Some(false);
        }
        if u32::from_ne_bytes(kind) == PT_INTERP {
            return Some(true);
        }
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The executables of this machine name an interpreter, and the objects that are only ever
    /// loaded do not. That difference is what the fallback turns on.
    #[test]
    fn a_program_names_an_interpreter_and_a_plain_shared_object_does_not() {
        let own = std::env::current_exe().expect("a test binary has a path");
        assert!(is_a_program(&own), "{} names no interpreter", own.display());
        assert!(!is_a_program(Path::new("/no/such/file")));
    }

    #[test]
    fn a_setting_is_the_name_joined_to_the_value() {
        let made = setting("LIBTIMEWARP_LOG", OsStr::new("/tmp/x")).unwrap();
        assert_eq!(made.to_bytes(), b"LIBTIMEWARP_LOG=/tmp/x");
        // A value with a NUL in it names nothing, so it makes no setting at all.
        assert_eq!(setting("A", OsStr::from_bytes(b"b\0c")), None);
    }
}
