//! The functions past ours: what the loader would have bound the program to had we never loaded.
//!
//! The clock group reaches the kernel by number, because those calls have one. A wait does not:
//! `pthread_cond_timedwait` and its neighbours are libc's own constructions over futexes, so the
//! genuine side comes from the loader, by name. Every name below is one we export ourselves, which
//! is why calling it directly would come straight back to us.
//!
//! Adding a function is one entry in [`genuine!`]: the slot, the lookup, and the wrapper all come
//! from there, and [`prime`] finds it with the rest at load.
//!
//! Linux's list, because the waits it takes over are Linux's; the other two arms bring their own.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicPtr, Ordering};

use common::platform::genuine_symbol;

/// Declares the genuine side of an interposed function: a slot for the loader's answer, and a
/// wrapper that calls it.
///
/// `None` back means this libc defines no such function, which only a program calling a name its
/// own libc never had can reach. The caller reports that the way its own function reports a
/// failure.
macro_rules! genuine {
    ($(
        $(#[$doc:meta])*
        fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty;
    )*) => {
        $(
            mod $name {
                //! One genuine function: where the loader's answer for it is kept.
                use super::{AtomicPtr, c_void, ptr};

                /// The definition past ours, or null until a lookup finds it.
                pub(super) static FOUND: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

                /// The name the loader is asked for.
                pub(super) const SYMBOL: &[u8] =
                    concat!(stringify!($name), "\0").as_bytes();
            }

            $(#[$doc])*
            ///
            /// # Safety
            /// The arguments must satisfy the contract libc gives the function of this name.
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
        )*

        /// Finds every one of them now, from the constructor.
        ///
        /// A lookup takes the loader's own lock, and the first call to an interposed wait can come
        /// from anywhere, a library's constructor included. Doing them all here leaves nothing for
        /// such a call to find missing.
        pub(crate) fn prime() {
            $( let _found = resolve(&$name::FOUND, $name::SYMBOL); )*
        }
    };
}

/// The definition past ours, remembered after the first lookup.
///
/// Relaxed both ways: the answer names a function in a library the loader mapped and relocated
/// before this process ran a line of its own, so there is nothing for an acquire to order against,
/// and reaching the code through the pointer is an address dependency. Two threads racing here
/// find the same address and store it twice.
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

    /// The genuine `thrd_sleep`, C11's own sleep.
    fn thrd_sleep(duration: *const libc::timespec, remaining: *mut libc::timespec) -> libc::c_int;

    /// The genuine `poll`.
    fn poll(fds: *mut libc::pollfd, nfds: libc::nfds_t, timeout: libc::c_int) -> libc::c_int;

    /// The genuine `ppoll`.
    fn ppoll(
        fds: *mut libc::pollfd,
        nfds: libc::nfds_t,
        timeout: *const libc::timespec,
        sigmask: *const libc::sigset_t,
    ) -> libc::c_int;

    /// The genuine `select`.
    fn select(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *mut libc::timeval,
    ) -> libc::c_int;

    /// The genuine `pselect`.
    fn pselect(
        nfds: libc::c_int,
        readfds: *mut libc::fd_set,
        writefds: *mut libc::fd_set,
        exceptfds: *mut libc::fd_set,
        timeout: *const libc::timespec,
        sigmask: *const libc::sigset_t,
    ) -> libc::c_int;

    /// The genuine `epoll_wait`.
    fn epoll_wait(
        epfd: libc::c_int,
        events: *mut libc::epoll_event,
        maxevents: libc::c_int,
        timeout: libc::c_int,
    ) -> libc::c_int;

    /// The genuine `epoll_pwait`.
    fn epoll_pwait(
        epfd: libc::c_int,
        events: *mut libc::epoll_event,
        maxevents: libc::c_int,
        timeout: libc::c_int,
        sigmask: *const libc::sigset_t,
    ) -> libc::c_int;

    /// The genuine `epoll_pwait2`, which libc has only from glibc 2.35.
    fn epoll_pwait2(
        epfd: libc::c_int,
        events: *mut libc::epoll_event,
        maxevents: libc::c_int,
        timeout: *const libc::timespec,
        sigmask: *const libc::sigset_t,
    ) -> libc::c_int;

    /// The genuine `pthread_cond_init`, which learns a condition variable's clock on the way past.
    fn pthread_cond_init(
        cond: *mut libc::pthread_cond_t,
        attr: *const libc::pthread_condattr_t,
    ) -> libc::c_int;

    /// The genuine `pthread_cond_destroy`.
    fn pthread_cond_destroy(cond: *mut libc::pthread_cond_t) -> libc::c_int;

    /// The genuine `pthread_cond_timedwait`.
    fn pthread_cond_timedwait(
        cond: *mut libc::pthread_cond_t,
        mutex: *mut libc::pthread_mutex_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `pthread_cond_clockwait`, which libc has only from glibc 2.30.
    fn pthread_cond_clockwait(
        cond: *mut libc::pthread_cond_t,
        mutex: *mut libc::pthread_mutex_t,
        clock: libc::clockid_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `pthread_mutex_timedlock`.
    fn pthread_mutex_timedlock(
        mutex: *mut libc::pthread_mutex_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `pthread_rwlock_timedrdlock`.
    fn pthread_rwlock_timedrdlock(
        rwlock: *mut libc::pthread_rwlock_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `pthread_rwlock_timedwrlock`.
    fn pthread_rwlock_timedwrlock(
        rwlock: *mut libc::pthread_rwlock_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `pthread_timedjoin_np`.
    fn pthread_timedjoin_np(
        thread: libc::pthread_t,
        retval: *mut *mut c_void,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `sem_timedwait`.
    fn sem_timedwait(sem: *mut libc::sem_t, abstime: *const libc::timespec) -> libc::c_int;

    /// The genuine `sem_clockwait`, which libc has only from glibc 2.30.
    fn sem_clockwait(
        sem: *mut libc::sem_t,
        clock: libc::clockid_t,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `sigtimedwait`.
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

    /// The genuine `recvmmsg`.
    fn recvmmsg(
        sockfd: libc::c_int,
        msgvec: *mut libc::mmsghdr,
        vlen: libc::c_uint,
        flags: libc::c_int,
        timeout: *mut libc::timespec,
    ) -> libc::c_int;

    /// The genuine `cnd_timedwait`. C11 spells its condition variable and mutex with types of its
    /// own, which we only ever pass along, so they travel as the opaque pointers they are.
    fn cnd_timedwait(
        cond: *mut c_void,
        mutex: *mut c_void,
        deadline: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `mtx_timedlock`, whose mutex is opaque for the reason `cnd_timedwait`'s is.
    fn mtx_timedlock(mutex: *mut c_void, deadline: *const libc::timespec) -> libc::c_int;

    /// The genuine `mq_timedsend`.
    fn mq_timedsend(
        mqdes: libc::mqd_t,
        msg: *const libc::c_char,
        len: libc::size_t,
        priority: libc::c_uint,
        abstime: *const libc::timespec,
    ) -> libc::c_int;

    /// The genuine `mq_timedreceive`.
    fn mq_timedreceive(
        mqdes: libc::mqd_t,
        msg: *mut libc::c_char,
        len: libc::size_t,
        priority: *mut libc::c_uint,
        abstime: *const libc::timespec,
    ) -> libc::ssize_t;
}
