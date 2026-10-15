//! The write lock: writer against writer.
//!
//! An exclusive kernel lock on `<name>.lock`, owned by the open file description one `open` call
//! creates. That ownership is what makes the lock answer the questions the protocol asks of it: a
//! holder's death releases it, nobody but the holder can drop it, and the kernel will say whether
//! it is held without handing out a pid.

use std::cell::Cell;
use std::ffi::CString;
use std::io;
use std::os::raw::c_char;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::platform;
use crate::session::layout::Header;
use crate::session::naming;

/// The one writer this process may have.
///
/// Re-locking through the same description converts the existing lock instead of blocking, so two
/// threads of one process would both believe they held it. This mutex is what makes the second one
/// wait. It covers the process rather than a single session, which costs a process writing two
/// sessions at once a wait it did not need and is otherwise the same lock.
static WRITERS: Mutex<()> = Mutex::new(());

/// One mapped session this process may read, and the lock file a reader of it probes.
///
/// Nodes are pushed onto [`PROBES`] and never unlinked or freed, so the fallback walks the list
/// with no lock and never meets freed memory. The path is leaked as well: a reader can be inside
/// `open` with the old pointer while a later mapping at the same address replaces it.
struct Probe {
    /// The address of the mapped header, which is what a reader holds.
    header: usize,
    /// The lock file, NUL-terminated and ready for `open`.
    path: AtomicPtr<c_char>,
    /// The node pushed before this one, or null. Set before this node is published.
    next: *const Self,
}

/// Every mapping [`install`] has named a lock file for, newest first.
static PROBES: AtomicPtr<Probe> = AtomicPtr::new(ptr::null_mut());

/// The node naming the lock file of the session mapped at `header`, if [`install`] made one.
///
/// A load per node and no writes, so the fallback can walk it inside a signal handler.
fn probe_for(header: &Header) -> Option<&'static Probe> {
    let key = ptr::from_ref(header).addr();
    let mut node = PROBES.load(Ordering::Acquire);
    // Safety: every node was leaked by `install`, is never freed, and had `next` set before the
    // release that published it.
    while let Some(probe) = unsafe { node.as_ref() } {
        if probe.header == key {
            return Some(probe);
        }
        node = probe.next.cast_mut();
    }
    None
}

/// The question every acquire and every probe asks: the write lock, byte 0, length 1.
///
/// Zeroed first, because Linux refuses a probe whose `l_pid` is not zero.
#[must_use]
pub fn lock_request() -> libc::flock {
    // Safety: `flock` is a plain-old-data struct, so an all-zero value is a valid one.
    let mut request: libc::flock = unsafe { std::mem::zeroed() };
    request.l_type = kind(libc::F_WRLCK);
    request.l_whence = kind(libc::SEEK_SET);
    request.l_start = 0;
    request.l_len = 1;
    request
}

/// `fcntl`'s lock kinds and whences live in a `c_short` field, and each platform declares the
/// constants at whatever width it likes: `c_int` on Linux, `c_short` on the BSDs. Widening to
/// `i64` first accepts either. Each is a small number everywhere we support, so nothing is lost.
fn kind<T: Into<i64>>(value: T) -> libc::c_short {
    libc::c_short::try_from(value.into()).unwrap_or(0)
}

/// Creates a lock file if it is missing, and checks it is one of ours.
///
/// A lock file carries no data, so creating one is a plain `O_CREAT` with nothing to initialize and
/// no creation race to lose. `0600`, because a wider mode would let another user take a shared lock
/// on it and block every writer for good.
///
/// # Errors
/// [`io::Error`] when the file cannot be created or opened, or is not one of ours.
pub fn create(path: &Path) -> io::Result<()> {
    drop(open_checked(path)?);
    Ok(())
}

/// Opens a lock file read-only, creating it if it is missing, and gives back the checked path.
fn open_checked(path: &Path) -> io::Result<CString> {
    let c_path = c_path(path)?;
    // Safety: `c_path` is a live NUL-terminated path.
    let fd = unsafe { libc::open(c_path.as_ptr(), READ_FLAGS | libc::O_CREAT, 0o600) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let checked = ours(fd);
    // Safety: we opened this descriptor and nothing else holds it.
    unsafe { libc::close(fd) };
    checked?;
    Ok(c_path)
}

/// Names the lock file a reader of the session mapped at `header` probes, creating it if it is
/// missing.
///
/// A relative `path` is taken against the working directory of this call, so the probe still finds
/// the file after the process changes directory.
///
/// Called once per mapping by whoever creates or attaches to the session, which is also where the
/// file's half of the session's security checks happens: a reader's probe is three syscalls and no
/// more, so the file it will open is vetted here instead, once.
///
/// The name belongs to the mapping, so a process holding several sessions probes, for each read,
/// the lock of the session that read is of. A mapping placed where an earlier one was replaces the
/// earlier one's name.
///
/// # Errors
/// [`io::Error`] when the file cannot be created or opened, or is not one of ours, or when `path`
/// is relative and the working directory cannot be learned. The mapping keeps whatever name it had.
pub fn install(header: &Header, path: &Path) -> io::Result<()> {
    let c_path = open_checked(&naming::absolute(path)?)?.into_raw();
    if let Some(probe) = probe_for(header) {
        probe.path.store(c_path, Ordering::Release);
        return Ok(());
    }
    let probe = Box::leak(Box::new(Probe {
        header: ptr::from_ref(header).addr(),
        path: AtomicPtr::new(c_path),
        next: ptr::null(),
    }));
    let mut head = PROBES.load(Ordering::Relaxed);
    loop {
        probe.next = head;
        match PROBES.compare_exchange_weak(head, probe, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => return Ok(()),
            Err(now) => head = now,
        }
    }
}

/// How a reader opens the lock file. `O_NOFOLLOW` refuses a planted symlink and `O_NONBLOCK` keeps
/// a planted FIFO from blocking us where we stand, which for the probe is wherever a clock read
/// can happen. No write access, because the question needs none.
const READ_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;

/// **R6**: whether the write lock of the session mapped at `header` is held, asked of the kernel
/// and taking nothing.
///
/// `None` when [`install`] named no lock file for this mapping, or the file cannot be asked about,
/// which a caller reads as an unanswered question rather than as a released lock.
///
/// A process holding the lock asks the kernel too, and hears held: the probe goes through a
/// description of its own, which the holder's description-owned lock conflicts with on every
/// platform, FreeBSD's `flock` against its `F_GETLK` included.
///
/// Async-signal-safe: a walk of loads, an `open`, one `fcntl`, and a `close`, which is every
/// syscall this makes. The file was vetted by [`install`], so nothing here has to. The descriptor
/// is opened here and closed before returning, so it is never a number the program can see, and a
/// daemon that closes every descriptor after startup leaves nothing of ours behind. Read-only,
/// because the question needs no write access, and the answer carries no pid, so pid recycling and
/// pid namespaces cannot make it wrong.
#[must_use]
pub fn probe_held(header: &Header) -> Option<bool> {
    let path = probe_for(header)?.path.load(Ordering::Acquire);
    // Safety: `path` is a NUL-terminated path leaked by `install`, so it outlives this call.
    let fd = unsafe { libc::open(path, READ_FLAGS) };
    if fd < 0 {
        return None;
    }
    let mut request = lock_request();
    // Safety: `fd` is open on the lock file and `request` is a live `flock`.
    let answered = unsafe { platform::probe_write_lock(fd, &raw mut request) } == 0;
    // Safety: we opened this descriptor and nothing else holds it.
    unsafe { libc::close(fd) };
    // One parser for all three platforms: any lock kind other than `F_UNLCK` means held. The pid
    // the kernel fills in is -1 for a description-owned lock and plays no part in the answer.
    answered.then(|| i64::from(request.l_type) != i64::from(libc::F_UNLCK))
}

/// **W1**: the write lock, held across the whole write flow.
///
/// Dropping it is **W12**: closing the descriptor drops the last reference to the description, and
/// the kernel releases the lock with it.
#[derive(Debug)]
pub struct WriteLock {
    /// The descriptor owning the lock, or -1 once it has been closed.
    fd: Cell<libc::c_int>,
    /// The in-process half of the lock, released with it.
    _writers: MutexGuard<'static, ()>,
}

impl WriteLock {
    /// Takes the write lock on `path`, blocking until the holder releases it.
    ///
    /// The wait has no bound: a held lock means a live writer that will finish.
    ///
    /// # Errors
    /// [`io::Error`] when the lock file cannot be opened, is not ours, or cannot be locked.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        // Above the file lock, so two threads of this process take turns rather than both
        // converting one description's lock.
        let writers = WRITERS.lock().unwrap_or_else(PoisonError::into_inner);
        let fd = open_for_writing(path)?;
        loop {
            // Safety: `fd` is open on the lock file, for writing.
            if unsafe { platform::take_write_lock(fd) } == 0 {
                break;
            }
            let err = io::Error::last_os_error();
            // A signal can interrupt the wait on every platform, including where the
            // documentation does not say so.
            if err.kind() != io::ErrorKind::Interrupted {
                // Safety: we opened this descriptor and nothing else holds it.
                unsafe { libc::close(fd) };
                return Err(err);
            }
        }
        Ok(Self {
            fd: Cell::new(fd),
            _writers: writers,
        })
    }

    /// Drops this process's reference to the lock without releasing it, for a fork that keeps
    /// running rather than exec'ing.
    ///
    /// An inherited descriptor keeps the description, and so the lock, alive past the writer's
    /// death, leaving readers waiting on a lock nobody will release. `O_CLOEXEC` covers a fork
    /// followed by an exec; this covers the rest.
    pub fn close_in_forked_child(&self) {
        let fd = self.fd.replace(-1);
        if fd >= 0 {
            // Safety: this child inherited the descriptor and nothing else here holds it.
            unsafe { libc::close(fd) };
        }
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        self.close_in_forked_child();
    }
}

/// Opens the lock file the way a writer must: writable, since a write lock needs a writable
/// descriptor, and checked to be a regular file of ours.
fn open_for_writing(path: &Path) -> io::Result<libc::c_int> {
    let c_path = c_path(path)?;
    // Safety: `c_path` is a live NUL-terminated path.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    match ours(fd) {
        Ok(()) => Ok(fd),
        Err(err) => {
            // Safety: we opened this descriptor and nothing else holds it.
            unsafe { libc::close(fd) };
            Err(err)
        }
    }
}

/// The lock file's half of the session's security checks: a regular file belonging to us.
fn ours(fd: libc::c_int) -> io::Result<()> {
    // Safety: `stat` is a plain-old-data struct, so an all-zero value is a valid one.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // Safety: `fd` is open and `st` is a live `stat`.
    if unsafe { libc::fstat(fd, &raw mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(io::Error::other("the lock file is not a regular file"));
    }
    // Safety: `geteuid` reads process state and cannot fail.
    if st.st_uid != unsafe { libc::geteuid() } {
        return Err(io::Error::other("the lock file belongs to another user"));
    }
    Ok(())
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::other("a path with a NUL in it names nothing"))
}
