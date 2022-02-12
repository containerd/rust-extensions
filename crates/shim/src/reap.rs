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

use std::io::Result;

#[cfg(target_os = "linux")]
/// Set current process as subreaper for child processes.
///
/// A subreaper fulfills the role of `init` for its descendant processes.  When a process becomes
/// orphaned (i.e., its immediate parent terminates), then that process will be reparented to the
/// nearest still living ancestor subreaper. Subsequently, calls to `getppid()` in the orphaned
/// process will now return the PID of the subreaper process, and when the orphan terminates,
/// it is the subreaper process that will receive a SIGCHLD signal and will be able to `wait()`
/// on the process to discover its termination status.
pub fn set_subreaper() -> Result<()> {
    use libc::PR_SET_CHILD_SUBREAPER;
    use std::io::Error;

    // Set current process as `subreaper` for child processes if the second parameter is non-zero,
    // otherwise unset the attribute.
    // Safe because we trust the kernel and have checked the result.
    let code = unsafe { libc::prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0) };
    if code != 0 {
        Err(Error::from_raw_os_error(code))
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
pub fn set_subreaper() -> Result<()> {
    Ok(())
}
