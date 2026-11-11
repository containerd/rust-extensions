/*
   Copyright The containerd Authors.

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
*/

//! Anchor process holding a PID namespace open.
//!
//! A Linux PID namespace has no content of its own: the kernel destroys it,
//! killing everything inside, the moment its PID 1 exits, and no new process
//! can be created in it afterwards. Unlike a network or IPC namespace it
//! therefore cannot be kept alive by a bind mount, and since
//! `unshare(CLONE_NEWPID)` does not move the caller into the new namespace —
//! only the caller's next child becomes its PID 1 — a thread cannot stand in
//! either. Something has to be PID 1 and stay there, which is all this
//! program does.
//!
//! The caller spawns it with `CLONE_NEWPID`, and it lives until the caller
//! sends it `SIGKILL` to tear the namespace down.
//!
//! This is `no_std` deliberately. The whole program is three signal
//! dispositions and a sleep, none of which needs anything from `std`, and
//! avoiding it keeps the binary small enough that exec'ing it costs
//! essentially nothing.
//!
//! `no_std` binaries require `panic = "abort"`, which the workspace sets only
//! for release builds. Builds that unwind, such as the default dev profile,
//! link `std` for its panic runtime instead; the program is otherwise the same.

#![cfg_attr(panic = "abort", no_std)]
#![no_main]

use core::{
    ffi::{c_char, c_int},
    ptr,
};

// The libc crate is used without its "std" feature, which is also what
// normally arranges for libc itself to be linked. Request it explicitly: the
// C runtime provides this program's entry point (see `main` below) as well as
// the handful of functions it calls.
#[link(name = "c")]
unsafe extern "C" {}

#[cfg(panic = "abort")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // Nothing here can meaningfully panic, and there is no unwinding with
    // panic = "abort". Failing loudly beats a PID 1 in an unknown state.
    unsafe { libc::abort() }
}

/// Installs `handler` for `signum` with `flags`, discarding the previous
/// disposition.
///
/// # Safety
///
/// `handler` must be a valid `sighandler_t` (`SIG_DFL`, `SIG_IGN`, or a
/// pointer to an async-signal-safe function).
unsafe fn set_disposition(signum: c_int, handler: libc::sighandler_t, flags: c_int) -> c_int {
    unsafe {
        let mut act: libc::sigaction = core::mem::zeroed();
        act.sa_sigaction = handler;
        act.sa_flags = flags;
        libc::sigemptyset(&mut act.sa_mask);
        libc::sigaction(signum, &act, ptr::null_mut())
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: c_int, _argv: *const *const c_char) -> c_int {
    unsafe {
        // Reap anything reparented here. A process elsewhere in the shared
        // namespace that outlives its original parent becomes our child, and
        // left unreaped those would pile up as zombies for the sandbox's whole
        // lifetime -- a PID 1 duty. SA_NOCLDWAIT has the kernel do it, so
        // there is no handler to write and no wait loop to run; the
        // alternative is a SIGCHLD handler calling waitpid(WNOHANG) until it
        // drains, which achieves the same thing with more moving parts.
        if set_disposition(libc::SIGCHLD, libc::SIG_DFL, libc::SA_NOCLDWAIT) != 0 {
            return 1;
        }

        // Refuse to die on anything catchable. PID 1 of a namespace is
        // already protected from default-disposition signals raised inside
        // that namespace, but not from ones sent by an ancestor namespace, so
        // ignoring these explicitly is what makes SIGKILL -- which is how the
        // namespace is deliberately torn down -- the only way out.
        if set_disposition(libc::SIGINT, libc::SIG_IGN, 0) != 0 {
            return 2;
        }
        if set_disposition(libc::SIGTERM, libc::SIG_IGN, 0) != 0 {
            return 3;
        }

        loop {
            // Sleeps until a signal arrives. Ignored signals and SIGCHLD
            // under SA_NOCLDWAIT do not wake it, so in practice this blocks
            // forever; the loop is here so that a spurious wakeup cannot turn
            // into an exit.
            libc::pause();
        }
    }
}
