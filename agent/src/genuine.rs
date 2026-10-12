//! The functions past ours: what the loader would have bound the program to had we never loaded.
//!
//! The clock group's genuine side is the platform seam's, which each platform reaches its own way.
//! A wait has no such side: `pthread_cond_timedwait` and its neighbours are libc's own
//! constructions over futexes, so the genuine side comes from the loader, by name. A timer is the
//! same: `timer_create` and its neighbours carry libc state of their own. Every name below is one
//! we export ourselves on Linux and FreeBSD, which is why calling it directly there would come
//! straight back to us.
//!
//! Adding a function is one entry in [`genuine!`]: the slot, the lookup, and the wrapper all come
//! from there, and `prime` finds it with the rest at load.
//!
//! The file timestamps come by name too. `stat` has no one system call across the architectures
//! we support, and the buffer libc fills is libc's own shape rather than the kernel's, so the
//! honest way past ours is the definition the loader would have bound.
//!
//! One list for all three platforms. The names only some of them have carry a `#[cfg]` of their
//! own: a name this libc never defined would be a lookup that can only fail, and a type it never
//! declared would not compile.
//!
//! macOS calls each name directly, the Mach clocks and the sleep measured in their ticks among
//! them. We export none of them there, and dyld binds the imports of the image that carries the
//! interpose table to the functions the table replaces, so the import is libSystem's definition and
//! there is nothing to look up. macOS also keeps more than one spelling of some functions on
//! `x86_64`, the suffix naming the ABI a program was compiled against, so an entry that calls one
//! names it with a `cfg_attr` setting the import's `link_name`. Which spellings the image actually
//! exports is a question for the build rather than an assumption; `build.rs` asks the linker.

#[cfg(not(target_os = "macos"))]
use std::ffi::c_void;
#[cfg(not(target_os = "macos"))]
use std::ptr::{self, NonNull};
#[cfg(not(target_os = "macos"))]
use std::sync::atomic::{AtomicPtr, Ordering};

#[cfg(not(target_os = "macos"))]
use common::platform::genuine_symbol;

/// Declares the genuine side of an interposed function, and a wrapper that calls it: through a
/// slot for the loader's answer on Linux and FreeBSD, and through an import on macOS.
///
/// `None` back means this libc defines no such function, which only a program calling a name its
/// own libc never had can reach. The caller reports that the way its own function reports a
/// failure. An import macOS lacks is a library dyld refuses to load instead, so there the answer
/// is always `Some`.
macro_rules! genuine {
    ($(
        $(#[doc = $doc:expr])*
        $(#[cfg($on:meta)])?
        $(#[cfg_attr($($spelling:tt)*)])*
        fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty;
    )*) => {
        $(
            $(#[cfg($on)])?
            #[cfg(not(target_os = "macos"))]
            mod $name {
                //! One genuine function: where the loader's answer for it is kept, and the lookup
                //! that fills it.
                use super::{AtomicPtr, c_void, ptr, resolve};

                /// The definition past ours, or null until a lookup finds it.
                pub(super) static FOUND: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

                /// The name the loader is asked for.
                pub(super) const SYMBOL: &[u8] = concat!(stringify!($name), "\0").as_bytes();

                /// Looks the name up now, so no later call has to.
                pub(super) fn prime() {
                    let _found = resolve(&FOUND, SYMBOL);
                }
            }

            $(#[doc = $doc])*
            ///
            /// # Safety
            /// The arguments must satisfy the contract libc gives the function of this name.
            $(#[cfg($on)])?
            #[cfg(not(target_os = "macos"))]
            #[inline]
            pub(crate) unsafe fn $name($($arg: $ty),*) -> Option<$ret> {
                /// What the loader's answer is called through.
                type Signature = unsafe extern "C" fn($($ty),*) -> $ret;
                let found = resolve(&$name::FOUND, $name::SYMBOL)?;
                // Safety: libc declares this name with this signature.
                let call = unsafe {
                    std::mem::transmute::<*mut c_void, Signature>(found.as_ptr())
                };
                // Safety: the caller guarantees the arguments, which pass on unchanged.
                Some(unsafe { call($($arg),*) })
            }

            $(#[doc = $doc])*
            ///
            /// # Safety
            /// The arguments must satisfy the contract libc gives the function of this name.
            $(#[cfg($on)])?
            #[cfg(target_os = "macos")]
            #[inline]
            pub(crate) unsafe fn $name($($arg: $ty),*) -> Option<$ret> {
                unsafe extern "C" {
                    $(#[cfg_attr($($spelling)*)])*
                    fn $name($($arg: $ty),*) -> $ret;
                }
                // Safety: the caller guarantees the arguments, which pass on unchanged.
                Some(unsafe { $name($($arg),*) })
            }
        )*

        /// Finds every one of them now, from the constructor.
        ///
        /// A lookup takes the loader's own lock, and the first call to an interposed wait can come
        /// from anywhere, a library's constructor included. Doing them all here leaves nothing for
        /// such a call to find missing.
        #[cfg(not(target_os = "macos"))]
        pub(crate) fn prime() {
            $( $(#[cfg($on)])? $name::prime(); )*
        }
    };
}

/// The definition past ours, remembered after the first lookup.
///
/// Relaxed both ways: the answer names a function in a library the loader mapped and relocated
/// before this process ran a line of its own, so there is nothing for an acquire to order against,
/// and reaching the code through the pointer is an address dependency. Two threads racing here
/// find the same address and store it twice.
#[cfg(not(target_os = "macos"))]
#[inline]
fn resolve(slot: &AtomicPtr<c_void>, name: &[u8]) -> Option<NonNull<c_void>> {
    if let Some(found) = NonNull::new(slot.load(Ordering::Relaxed)) {
        return Some(found);
    }
    // Safety: the name is NUL-terminated by construction.
    let found = unsafe { genuine_symbol(name) }?;
    slot.store(found.as_ptr(), Ordering::Relaxed);
    Some(found)
}

genuine! {
    /// The genuine `nanosleep`, which every relative sleep of ours blocks in.
    fn nanosleep(req: *const libc::timespec, rem: *mut libc::timespec) -> libc::c_int;

    /// The genuine `clock_nanosleep`, relative or absolute as the flags say.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn clock_nanosleep(
        clock: libc::clockid_t,
        flags: libc::c_int,
        req: *const libc::timespec,
        rem: *mut libc::timespec,
    ) -> libc::c_int;

    /// The genuine `sleep`, for a process with no session of ours.
    fn sleep(seconds: libc::c_uint) -> libc::c_uint;

    /// The genuine `usleep`, for a process with no session of ours.
    fn usleep(micros: libc::useconds_t) -> libc::c_int;

    /// The genuine `thrd_sleep`, C11's own sleep. Linux's alone: FreeBSD builds the call out of
    /// the public `nanosleep`, which we take over instead.
    #[cfg(target_os = "linux")]
    fn thrd_sleep(duration: *const libc::timespec, remaining: *mut libc::timespec) -> libc::c_int;

    /// The genuine `poll`.
    fn poll(fds: *mut libc::pollfd, nfds: libc::nfds_t, timeout: libc::c_int) -> libc::c_int;

    /// The genuine `ppoll`.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn ppoll(
        fds: *mut libc::pollfd,
        nfds: libc::nfds_t,
        timeout: *const libc::timespec,
        sigmask: *const libc::sigset_t,
    ) -> libc::c_int;

    /// The genuine `select`. macOS on `x86_64` keeps three spellings, and a call the program wrote
    /// in any of them is answered here by the widest this build found, the one that refuses the
    /// fewest descriptor sets, which leaves a caller no worse off than without us.
    #[cfg_attr(darwin_select_extsn, link_name = "select$DARWIN_EXTSN")]
    #[cfg_attr(
        all(darwin_select_1050, not(darwin_select_extsn)),
        link_name = "select$1050"
    )]
    fn select(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *mut libc::timeval,
    ) -> libc::c_int;

    /// The genuine `pselect`, in the wider of the two spellings macOS on `x86_64` keeps.
    #[cfg_attr(darwin_pselect_extsn, link_name = "pselect$DARWIN_EXTSN")]
    fn pselect(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *const libc::timespec,
        sigmask: *const libc::sigset_t,
    ) -> libc::c_int;

    /// The genuine `epoll_wait`.
    #[cfg(target_os = "linux")]
    fn epoll_wait(
        epfd: libc::c_int,
        events: *mut libc::epoll_event,
        maxevents: libc::c_int,
        timeout: libc::c_int,
    ) -> libc::c_int;

    /// The genuine `epoll_pwait`.
    #[cfg(target_os = "linux")]
    fn epoll_pwait(
        epfd: libc::c_int,
        events: *mut libc::epoll_event,
        maxevents: libc::c_int,
        timeout: libc::c_int,
        sigmask: *const libc::sigset_t,
    ) -> libc::c_int;

    /// The genuine `epoll_pwait2`, which libc has only from glibc 2.35.
    #[cfg(target_os = "linux")]
    fn epoll_pwait2(
        epfd: libc::c_int,
        events: *mut libc::epoll_event,
        maxevents: libc::c_int,
        timeout: *const libc::timespec,
        sigmask: *const libc::sigset_t,
    ) -> libc::c_int;

    /// The genuine `pthread_cond_init`, which learns a condition variable's clock on the way past.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn pthread_cond_init(
        cond: *mut libc::pthread_cond_t,
        attr: *const libc::pthread_condattr_t,
    ) -> libc::c_int;

    /// The genuine `pthread_cond_destroy`.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn pthread_cond_destroy(cond: *mut libc::pthread_cond_t) -> libc::c_int;

    /// The genuine `pthread_cond_timedwait`.
    fn pthread_cond_timedwait(
        cond: *mut libc::pthread_cond_t,
        mutex: *mut libc::pthread_mutex_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `pthread_cond_clockwait`, which libc has only from glibc 2.30.
    #[cfg(target_os = "linux")]
    fn pthread_cond_clockwait(
        cond: *mut libc::pthread_cond_t,
        mutex: *mut libc::pthread_mutex_t,
        clock: libc::clockid_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `pthread_mutex_timedlock`.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn pthread_mutex_timedlock(
        mutex: *mut libc::pthread_mutex_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `pthread_rwlock_timedrdlock`.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn pthread_rwlock_timedrdlock(
        rwlock: *mut libc::pthread_rwlock_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `pthread_rwlock_timedwrlock`.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn pthread_rwlock_timedwrlock(
        rwlock: *mut libc::pthread_rwlock_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `pthread_timedjoin_np`.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn pthread_timedjoin_np(
        thread: libc::pthread_t,
        retval: *mut *mut c_void,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `sem_timedwait`.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn sem_timedwait(sem: *mut libc::sem_t, abstime: *const libc::timespec) -> libc::c_int;

    /// The genuine `sem_clockwait`, which libc has only from glibc 2.30.
    #[cfg(target_os = "linux")]
    fn sem_clockwait(
        sem: *mut libc::sem_t,
        clock: libc::clockid_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `sigtimedwait`.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn sigtimedwait(
        set: *const libc::sigset_t,
        info: *mut libc::siginfo_t,
        timeout: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `aio_suspend`.
    fn aio_suspend(
        list: *const *const libc::aiocb,
        nent: libc::c_int,
        timeout: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `kevent`, the BSDs' own way of waiting on a set of events with a deadline.
    #[cfg(any(target_os = "freebsd", target_os = "macos"))]
    fn kevent(
        kq: libc::c_int,
        changelist: *const libc::kevent,
        nchanges: libc::c_int,
        eventlist: *mut libc::kevent,
        nevents: libc::c_int,
        timeout: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `kevent64`, the same wait over the wider event this platform added beside it.
    #[cfg(target_os = "macos")]
    fn kevent64(
        kq: libc::c_int,
        changelist: *const libc::kevent64_s,
        nchanges: libc::c_int,
        eventlist: *mut libc::kevent64_s,
        nevents: libc::c_int,
        flags: libc::c_uint,
        timeout: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `kevent_qos`, whose events travel as the blocks of bytes they are: nothing here
    /// reads one, because the call names no deadline for us to convert.
    #[cfg(darwin_kevent_qos)]
    fn kevent_qos(
        kq: libc::c_int,
        changelist: *const std::ffi::c_void,
        nchanges: libc::c_int,
        eventlist: *mut std::ffi::c_void,
        nevents: libc::c_int,
        data_out: *mut std::ffi::c_void,
        data_available: *mut libc::size_t,
        flags: libc::c_uint,
    ) -> libc::c_int;

    /// The genuine `mach_absolute_time`, ticks of the counter that stops while the machine sleeps.
    /// Every macOS has had it, so it is asked for unconditionally where the three below are not.
    #[cfg(target_os = "macos")]
    fn mach_absolute_time() -> u64;

    /// The genuine `mach_approximate_time`, the same counter read from the last tick.
    #[cfg(darwin_mach_approximate_time)]
    fn mach_approximate_time() -> u64;

    /// The genuine `mach_continuous_time`, ticks of the counter that counts on while it sleeps.
    #[cfg(darwin_mach_continuous_time)]
    fn mach_continuous_time() -> u64;

    /// The genuine `mach_continuous_approximate_time`.
    #[cfg(darwin_mach_continuous_approximate_time)]
    fn mach_continuous_approximate_time() -> u64;

    /// The genuine `clock_gettime_nsec_np`, a clock read in nanoseconds rather than a `timespec`.
    /// Zero is its refusal, with the reason in `errno`, which is why a clock nothing fakes comes
    /// here rather than being answered as nothing.
    #[cfg(target_os = "macos")]
    fn clock_gettime_nsec_np(clock: libc::clockid_t) -> u64;

    /// The genuine `mach_wait_until`, the one sleep this platform measures in ticks.
    #[cfg(target_os = "macos")]
    fn mach_wait_until(deadline: u64) -> libc::kern_return_t;

    /// The genuine `host_get_clock_service`, which hands out a port naming one of the Mach clocks.
    #[cfg(target_os = "macos")]
    fn host_get_clock_service(
        host: libc::host_t,
        id: libc::c_int,
        port: *mut libc::mach_port_t,
    ) -> libc::kern_return_t;

    /// The genuine `clock_get_time`, a read on such a port. Its buffer is Mach's own narrower pair,
    /// which [`crate::mach`] declares.
    #[cfg(target_os = "macos")]
    fn clock_get_time(
        port: libc::mach_port_t,
        at: *mut crate::mach::MachTimespec,
    ) -> libc::kern_return_t;

    /// The genuine `recvmmsg`. Linux's alone, for the reason `thrd_sleep` is: FreeBSD builds the
    /// call out of the public `ppoll`.
    #[cfg(target_os = "linux")]
    fn recvmmsg(
        sockfd: libc::c_int,
        msgvec: *mut libc::mmsghdr,
        vlen: libc::c_uint,
        flags: libc::c_int,
        timeout: *mut libc::timespec,
    ) -> libc::c_int;

    /// The genuine `cnd_timedwait`. C11 spells its condition variable and mutex with types of its
    /// own, which we only ever pass along, so they travel as the opaque pointers they are. Linux's
    /// alone, for the reason `thrd_sleep` is: FreeBSD builds the call out of the public
    /// `pthread_cond_timedwait`.
    #[cfg(target_os = "linux")]
    fn cnd_timedwait(
        cond: *mut c_void,
        mutex: *mut c_void,
        deadline: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `mtx_timedlock`, whose mutex is opaque for the reason `cnd_timedwait`'s is, and
    /// Linux's alone for the reason it is: FreeBSD builds the call out of the public
    /// `pthread_mutex_timedlock`.
    #[cfg(target_os = "linux")]
    fn mtx_timedlock(mutex: *mut c_void, deadline: *const libc::timespec) -> libc::c_int;

    /// The genuine `mq_timedsend`.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn mq_timedsend(
        mqdes: libc::mqd_t,
        msg: *const libc::c_char,
        len: libc::size_t,
        priority: libc::c_uint,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `mq_timedreceive`.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn mq_timedreceive(
        mqdes: libc::mqd_t,
        msg: *mut libc::c_char,
        len: libc::size_t,
        priority: *mut libc::c_uint,
        abstime: *const libc::timespec,
    ) -> libc::ssize_t;

    /// The genuine `setitimer`, which every interval timer of ours is armed through, `alarm`
    /// included.
    fn setitimer(
        which: libc::c_int,
        new_value: *const libc::itimerval,
        old_value: *mut libc::itimerval,
    ) -> libc::c_int;

    /// The genuine `getitimer`.
    fn getitimer(which: libc::c_int, curr_value: *mut libc::itimerval) -> libc::c_int;

    /// The genuine `timer_create`, which reads no time and hands back the id we record a clock
    /// against.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn timer_create(
        clock: libc::clockid_t,
        sevp: *mut libc::sigevent,
        timerid: *mut libc::timer_t,
    ) -> libc::c_int;

    /// The genuine `timer_settime`.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn timer_settime(
        timerid: libc::timer_t,
        flags: libc::c_int,
        new_value: *const libc::itimerspec,
        old_value: *mut libc::itimerspec,
    ) -> libc::c_int;

    /// The genuine `timer_gettime`.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn timer_gettime(timerid: libc::timer_t, curr_value: *mut libc::itimerspec) -> libc::c_int;

    /// The genuine `timer_delete`, which reads no time either.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn timer_delete(timerid: libc::timer_t) -> libc::c_int;

    /// The genuine `timerfd_create`, which reads no time either.
    #[cfg(target_os = "linux")]
    fn timerfd_create(clock: libc::clockid_t, flags: libc::c_int) -> libc::c_int;

    /// The genuine `timerfd_settime`.
    #[cfg(target_os = "linux")]
    fn timerfd_settime(
        fd: libc::c_int,
        flags: libc::c_int,
        new_value: *const libc::itimerspec,
        old_value: *mut libc::itimerspec,
    ) -> libc::c_int;

    /// The genuine `timerfd_gettime`.
    #[cfg(target_os = "linux")]
    fn timerfd_gettime(fd: libc::c_int, curr_value: *mut libc::itimerspec) -> libc::c_int;

    /// The genuine `stat`, which glibc exports under this name only from 2.33 and the BSDs' libc
    /// has always exported. macOS on `x86_64` spells the buffer this libc declares under a suffix,
    /// and the unsuffixed name there fills a narrower buffer of another shape; the three below
    /// follow it.
    #[cfg_attr(darwin_inode64, link_name = "stat$INODE64")]
    fn stat(path: *const libc::c_char, buf: *mut libc::stat) -> libc::c_int;

    /// The genuine `stat64`.
    #[cfg(target_os = "linux")]
    fn stat64(path: *const libc::c_char, buf: *mut libc::stat64) -> libc::c_int;

    /// The genuine `fstat`.
    #[cfg_attr(darwin_inode64, link_name = "fstat$INODE64")]
    fn fstat(fd: libc::c_int, buf: *mut libc::stat) -> libc::c_int;

    /// The genuine `fstat64`.
    #[cfg(target_os = "linux")]
    fn fstat64(fd: libc::c_int, buf: *mut libc::stat64) -> libc::c_int;

    /// The genuine `lstat`.
    #[cfg_attr(darwin_inode64, link_name = "lstat$INODE64")]
    fn lstat(path: *const libc::c_char, buf: *mut libc::stat) -> libc::c_int;

    /// The genuine `lstat64`.
    #[cfg(target_os = "linux")]
    fn lstat64(path: *const libc::c_char, buf: *mut libc::stat64) -> libc::c_int;

    /// The genuine `fstatat`.
    #[cfg_attr(darwin_inode64, link_name = "fstatat$INODE64")]
    fn fstatat(
        dirfd: libc::c_int,
        path: *const libc::c_char,
        buf: *mut libc::stat,
        flags: libc::c_int,
    ) -> libc::c_int;

    /// The genuine `fstatat64`.
    #[cfg(target_os = "linux")]
    fn fstatat64(
        dirfd: libc::c_int,
        path: *const libc::c_char,
        buf: *mut libc::stat64,
        flags: libc::c_int,
    ) -> libc::c_int;

    /// The genuine `statx`, which libc has only from glibc 2.28. Its buffer is the kernel's own
    /// shape, which [`crate::stamp`] declares, so it travels here as the block of bytes it is.
    #[cfg(target_os = "linux")]
    fn statx(
        dirfd: libc::c_int,
        path: *const libc::c_char,
        flags: libc::c_int,
        mask: libc::c_uint,
        buf: *mut c_void,
    ) -> libc::c_int;

    /// The genuine `__xstat`, the name glibc before 2.33 routes `stat` through. Still exported by
    /// a newer one, for the programs built against an older one, which is what makes it the way
    /// back to the genuine call on both.
    #[cfg(target_os = "linux")]
    fn __xstat(
        version: libc::c_int,
        path: *const libc::c_char,
        buf: *mut libc::stat,
    ) -> libc::c_int;

    /// The genuine `__xstat64`.
    #[cfg(target_os = "linux")]
    fn __xstat64(
        version: libc::c_int,
        path: *const libc::c_char,
        buf: *mut libc::stat64,
    ) -> libc::c_int;

    /// The genuine `__fxstat`.
    #[cfg(target_os = "linux")]
    fn __fxstat(version: libc::c_int, fd: libc::c_int, buf: *mut libc::stat) -> libc::c_int;

    /// The genuine `__fxstat64`.
    #[cfg(target_os = "linux")]
    fn __fxstat64(version: libc::c_int, fd: libc::c_int, buf: *mut libc::stat64) -> libc::c_int;

    /// The genuine `__lxstat`.
    #[cfg(target_os = "linux")]
    fn __lxstat(
        version: libc::c_int,
        path: *const libc::c_char,
        buf: *mut libc::stat,
    ) -> libc::c_int;

    /// The genuine `__lxstat64`.
    #[cfg(target_os = "linux")]
    fn __lxstat64(
        version: libc::c_int,
        path: *const libc::c_char,
        buf: *mut libc::stat64,
    ) -> libc::c_int;

    /// The genuine `__fxstatat`.
    #[cfg(target_os = "linux")]
    fn __fxstatat(
        version: libc::c_int,
        dirfd: libc::c_int,
        path: *const libc::c_char,
        buf: *mut libc::stat,
        flags: libc::c_int,
    ) -> libc::c_int;

    /// The genuine `__fxstatat64`.
    #[cfg(target_os = "linux")]
    fn __fxstatat64(
        version: libc::c_int,
        dirfd: libc::c_int,
        path: *const libc::c_char,
        buf: *mut libc::stat64,
        flags: libc::c_int,
    ) -> libc::c_int;

    /// The genuine `utime`. Linux's alone: FreeBSD's libc builds the call out of the public
    /// `utimes`, which we take over instead, and macOS's set goes through `utimes` as well.
    #[cfg(target_os = "linux")]
    fn utime(path: *const libc::c_char, times: *const libc::utimbuf) -> libc::c_int;

    /// The genuine `utimes`.
    fn utimes(path: *const libc::c_char, times: *const libc::timeval) -> libc::c_int;

    /// The genuine `futimes`.
    fn futimes(fd: libc::c_int, times: *const libc::timeval) -> libc::c_int;

    /// The genuine `lutimes`. macOS's set is built out of the calls libSystem's own makes
    /// instead, since one of them is a name we take over.
    #[cfg(not(target_os = "macos"))]
    fn lutimes(path: *const libc::c_char, times: *const libc::timeval) -> libc::c_int;

    /// The genuine `utimensat`.
    fn utimensat(
        dirfd: libc::c_int,
        path: *const libc::c_char,
        times: *const libc::timespec,
        flags: libc::c_int,
    ) -> libc::c_int;

    /// The genuine `futimens`.
    fn futimens(fd: libc::c_int, times: *const libc::timespec) -> libc::c_int;
}
