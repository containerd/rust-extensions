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

// Forked from https://github.com/pwFoo/rust-runc/blob/313e6ae5a79b54455b0a242a795c69adf035141a/src/lib.rs

/*
 * Copyright 2020 fsyncd, Berlin, Germany.
 * Additional material, copyright of the containerd authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#![cfg_attr(docsrs, doc = include_str!("../README.md"))]

//! A crate for consuming the runc binary in your Rust applications, similar to
//! [go-runc](https://github.com/containerd/go-runc) for Go.
#[cfg(all(not(feature = "async"), target_os = "linux"))]
use std::os::unix::process::CommandExt;
use std::{
    fmt::{self, Debug, Display},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::Arc,
};

use log::debug;
use oci_spec::runtime::{LinuxResources, Process};
use serde::de::DeserializeOwned;

#[cfg(feature = "async")]
pub use crate::asynchronous::*;
#[cfg(not(feature = "async"))]
pub use crate::synchronous::*;
use crate::{
    container::Container,
    error::Error,
    options::*,
    utils::{abs_string, write_value_to_temp_file},
};

#[cfg(feature = "async")]
pub mod asynchronous;
pub mod container;
pub mod error;
pub mod events;
#[cfg(not(feature = "async"))]
pub mod synchronous;

#[cfg(feature = "async")]
pub mod monitor;
pub mod options;
pub mod utils;

const JSON: &str = "json";
const TEXT: &str = "text";

pub type Result<T> = std::result::Result<T, crate::error::Error>;

/// Response is for (pid, exit status, outputs).
#[derive(Debug, Clone)]
pub struct Response {
    pub pid: u32,
    pub status: ExitStatus,
    pub output: String,
}

#[derive(Debug, Clone)]
pub struct Version {
    pub runc_version: Option<String>,
    pub spec_version: Option<String>,
    pub commit: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub enum LogFormat {
    Json,
    #[default]
    Text,
}

impl Display for LogFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogFormat::Json => write!(f, "{}", JSON),
            LogFormat::Text => write!(f, "{}", TEXT),
        }
    }
}

#[cfg(not(feature = "async"))]
pub type Command = std::process::Command;

#[cfg(feature = "async")]
pub type Command = tokio::process::Command;

/// What a command's captured output should contain.
///
/// An enum rather than a `bool` because getting it wrong is silent: folding stderr
/// into stdout corrupts any output that is then deserialized, but only once runc
/// happens to emit a warning.
#[derive(Debug, Clone, Copy)]
enum Output {
    /// stdout only. Required whenever the output is deserialized.
    Stdout,
    /// stdout followed by stderr, for output only surfaced to the caller.
    Combined,
}

/// What to do with an Io driver's write ends once runc has finished.
#[derive(Debug, Clone, Copy)]
enum CloseIo {
    Close,
    /// Leave them open: `run` blocks for the container's lifetime, so they are the
    /// caller's to close.
    Keep,
}

/// runc emits a bare `null` rather than `[]` for an empty list, courtesy of Go.
fn parse_json_list<T: DeserializeOwned>(output: &str) -> Result<Vec<T>> {
    Ok(serde_json::from_str::<Option<Vec<T>>>(output.trim())?.unwrap_or_default())
}

#[derive(Debug, Clone)]
pub struct Runc {
    command: PathBuf,
    args: Vec<String>,
    spawner: Arc<dyn Spawner + Send + Sync>,
}

// Sync/async bridging
//
// The client is written once and compiled as either the blocking or the async
// client: `runc_impl!` is invoked with the `async` keyword or with nothing, and
// `maybe_await!` expands to `.await` or to nothing. The macro interpolates the
// keyword rather than parsing signatures, so generics, where-clauses and lifetimes
// all pass through untouched.
//
// Anything that genuinely differs between the two lives in a flavor-specific
// `impl Runc` block under `synchronous/` or `asynchronous/` instead.

/// Await an expression in the async build; evaluate it as-is in the sync build.
#[cfg(feature = "async")]
macro_rules! maybe_await {
    ($e:expr) => {
        $e.await
    };
}

#[cfg(not(feature = "async"))]
macro_rules! maybe_await {
    ($e:expr) => {
        $e
    };
}

macro_rules! runc_impl {
    ($($maybe_async:ident)?) => {
impl Runc {
    /// Build, spawn and collect one `runc` invocation.
    ///
    /// The whole execution path lives here, so it applies to every [Spawner],
    /// including custom ones installed via [options::GlobalOpts::custom_spawner].
    $($maybe_async)? fn launch_io(
        &self,
        args: &[String],
        output: Output,
        io: Option<&dyn Io>,
        close: CloseIo,
    ) -> Result<Response> {
        let mut cmd = Command::new(&self.command);

        // Default to piped stdio; an Io driver may override them below.
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // NOTIFY_SOCKET introduces a special behavior in runc but should only be set
        // if invoked from systemd.
        cmd.args([&self.args, args].concat())
            .env_remove("NOTIFY_SOCKET");

        // Only registered when the knob is actually set: any `pre_exec` costs the
        // `posix_spawn` fast path. See `utils::restore_thp` for why it must run in
        // the child rather than here.
        #[cfg(target_os = "linux")]
        if let Some(hook) = utils::restore_thp() {
            // SAFETY: the hook is async-signal-safe — one `prctl`, no locks.
            unsafe { cmd.pre_exec(hook) };
        }

        if let Some(io) = io {
            maybe_await!(io.set(&mut cmd)).map_err(|e| Error::IoSet(e.to_string()))?;
        }

        debug!("Execute command {:?}", cmd);
        let res = maybe_await!(self.spawner.execute(cmd));

        if let (Some(io), CloseIo::Close) = (io, close) {
            maybe_await!(io.close_after_start());
        }

        let (status, pid, stdout, stderr) = res?;
        if !status.success() {
            return Err(Error::CommandFailed { status, stdout, stderr });
        }
        let output = match output {
            Output::Stdout => stdout,
            Output::Combined => stdout + stderr.as_str(),
        };
        Ok(Response { pid, status, output })
    }

    /// [Self::launch_io] for the commands that attach no Io driver.
    $($maybe_async)? fn launch(&self, args: &[String], output: Output) -> Result<Response> {
        maybe_await!(self.launch_io(args, output, None, CloseIo::Keep))
    }

    /// Shared body of [Self::create] and [Self::run], which differ only in the
    /// subcommand and in who owns the Io driver's write ends afterwards.
    $($maybe_async)? fn launch_bundle<P>(
        &self,
        subcommand: &str,
        id: &str,
        bundle: P,
        opts: Option<&CreateOpts>,
        close: CloseIo,
    ) -> Result<Response>
    where
        P: AsRef<Path>,
    {
        let mut args = vec![
            subcommand.to_string(),
            "--bundle".to_string(),
            abs_string(bundle)?,
        ];
        if let Some(opts) = opts {
            args.append(&mut opts.args()?);
        }
        args.push(id.to_string());
        let io = opts.and_then(|o| o.io.as_deref());
        maybe_await!(self.launch_io(&args, Output::Combined, io, close))
    }

    /// Create a new container
    pub $($maybe_async)? fn create<P>(
        &self,
        id: &str,
        bundle: P,
        opts: Option<&CreateOpts>,
    ) -> Result<Response>
    where
        P: AsRef<Path>,
    {
        maybe_await!(self.launch_bundle("create", id, bundle, opts, CloseIo::Close))
    }

    /// Run the create, start, delete lifecycle of the container and return its exit status
    pub $($maybe_async)? fn run<P>(
        &self,
        id: &str,
        bundle: P,
        opts: Option<&CreateOpts>,
    ) -> Result<Response>
    where
        P: AsRef<Path>,
    {
        maybe_await!(self.launch_bundle("run", id, bundle, opts, CloseIo::Keep))
    }

    /// Delete a container
    pub $($maybe_async)? fn delete(&self, id: &str, opts: Option<&DeleteOpts>) -> Result<()> {
        let mut args = vec!["delete".to_string()];
        if let Some(opts) = opts {
            args.append(&mut opts.args());
        }
        args.push(id.to_string());
        maybe_await!(self.launch(&args, Output::Combined))?;
        Ok(())
    }

    /// Return an event stream of container notifications
    pub $($maybe_async)? fn events(&self, _id: &str, _interval: &std::time::Duration) -> Result<()> {
        Err(Error::Unimplemented("events".to_string()))
    }

    /// Execute an additional process inside the container
    pub $($maybe_async)? fn exec(
        &self,
        id: &str,
        spec: &Process,
        opts: Option<&ExecOpts>,
    ) -> Result<()> {
        // `_spec_file` owns the temp file and unlinks it when this call returns.
        // Note that on cancellation the file is unlinked while a spawned runc may
        // still be running (tokio does not kill children on drop); that is a narrow
        // window, and the alternative the old code took was to leak the file instead.
        let (_spec_file, path) = write_value_to_temp_file(spec)?;
        let mut args = vec!["exec".to_string(), "--process".to_string(), path];
        if let Some(opts) = opts {
            args.append(&mut opts.args()?);
        }
        args.push(id.to_string());
        let io = opts.and_then(|o| o.io.as_deref());
        maybe_await!(self.launch_io(&args, Output::Combined, io, CloseIo::Close))?;
        Ok(())
    }

    /// Send the specified signal to processes inside the container
    pub $($maybe_async)? fn kill(
        &self,
        id: &str,
        sig: u32,
        opts: Option<&KillOpts>,
    ) -> Result<()> {
        let mut args = vec!["kill".to_string()];
        if let Some(opts) = opts {
            args.append(&mut opts.args());
        }
        args.push(id.to_string());
        args.push(sig.to_string());
        maybe_await!(self.launch(&args, Output::Combined))?;
        Ok(())
    }

    /// List all containers associated with this runc instance
    pub $($maybe_async)? fn list(&self) -> Result<Vec<Container>> {
        let args = ["list".to_string(), "--format=json".to_string()];
        let res = maybe_await!(self.launch(&args, Output::Stdout))?;
        parse_json_list(&res.output)
    }

    /// Pause a container
    pub $($maybe_async)? fn pause(&self, id: &str) -> Result<()> {
        let args = ["pause".to_string(), id.to_string()];
        maybe_await!(self.launch(&args, Output::Combined))?;
        Ok(())
    }

    /// List all the processes inside the container, returning their pids
    pub $($maybe_async)? fn ps(&self, id: &str) -> Result<Vec<usize>> {
        let args = [
            "ps".to_string(),
            "--format=json".to_string(),
            id.to_string(),
        ];
        let res = maybe_await!(self.launch(&args, Output::Stdout))?;
        parse_json_list(&res.output)
    }

    /// Resume a container
    pub $($maybe_async)? fn resume(&self, id: &str) -> Result<()> {
        let args = ["resume".to_string(), id.to_string()];
        maybe_await!(self.launch(&args, Output::Combined))?;
        Ok(())
    }

    /// Start an already created container
    pub $($maybe_async)? fn start(&self, id: &str) -> Result<Response> {
        let args = ["start".to_string(), id.to_string()];
        maybe_await!(self.launch(&args, Output::Combined))
    }

    /// Return the state of a container
    pub $($maybe_async)? fn state(&self, id: &str) -> Result<Container> {
        let args = ["state".to_string(), id.to_string()];
        let res = maybe_await!(self.launch(&args, Output::Stdout))?;
        Ok(serde_json::from_str(res.output.trim())?)
    }

    /// Return the latest statistics for a container
    pub $($maybe_async)? fn stats(&self, id: &str) -> Result<events::Stats> {
        let args = ["events".to_string(), "--stats".to_string(), id.to_string()];
        let res = maybe_await!(self.launch(&args, Output::Stdout))?;
        let event: events::Event = serde_json::from_str(res.output.trim())?;
        event.stats.ok_or(Error::MissingContainerStats)
    }

    /// Update a container with the provided resource spec
    pub $($maybe_async)? fn update(&self, id: &str, resources: &LinuxResources) -> Result<()> {
        let (_spec_file, path) = write_value_to_temp_file(resources)?;
        let args = [
            "update".to_string(),
            "--resources".to_string(),
            path,
            id.to_string(),
        ];
        maybe_await!(self.launch(&args, Output::Combined))?;
        Ok(())
    }

    pub $($maybe_async)? fn checkpoint(&self) -> Result<()> {
        Err(Error::Unimplemented("checkpoint".to_string()))
    }

    pub $($maybe_async)? fn restore(&self) -> Result<()> {
        Err(Error::Unimplemented("restore".to_string()))
    }
}
    };
}

#[cfg(feature = "async")]
runc_impl!(async);
#[cfg(not(feature = "async"))]
runc_impl!();

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use std::sync::Arc;

    use oci_spec::runtime::Process;

    use crate::{
        error::Error,
        io::{InheritedStdIo, PipedStdIo},
        options::{CreateOpts, DeleteOpts, ExecOpts, GlobalOpts},
        Runc,
    };

    fn client(command: &str) -> Runc {
        GlobalOpts::new()
            .command(command)
            .build()
            .expect("unable to create runc instance")
    }

    /// Always succeeds with no output.
    fn ok_client() -> Runc {
        client("/bin/true")
    }

    /// Always exits 1 with no output.
    fn fail_client() -> Runc {
        client("/bin/false")
    }

    /// Echoes its arguments, so the captured output is non-empty.
    fn echo_client() -> Runc {
        client("/bin/echo")
    }

    fn dummy_process() -> Process {
        serde_json::from_str(r#"{ "user": { "uid": 1000, "gid": 1000 }, "cwd": "/path/to/dir" }"#)
            .unwrap()
    }

    /// Assert a call against [fail_client] surfaced the exit status rather than succeeding.
    fn assert_command_failed<T: std::fmt::Debug>(res: crate::Result<T>) {
        match res {
            Ok(v) => panic!("fail_client unexpectedly succeeded: {:?}", v),
            Err(Error::CommandFailed {
                status,
                stdout,
                stderr,
            }) => {
                assert_eq!(status.code(), Some(1));
                assert!(stdout.is_empty(), "unexpected stdout: {}", stdout);
                assert!(stderr.is_empty(), "unexpected stderr: {}", stderr);
            }
            Err(e) => panic!("unexpected error from fail_client: {:?}", e),
        }
    }

    /// The command tests are identical for both clients apart from `.await`, so they
    /// are written once here and expanded with the right test attribute per flavor.
    macro_rules! runc_tests {
        ($test:meta $(, $maybe_async:ident)?) => {
            #[$test]
            $($maybe_async)? fn test_create() {
                let opts = CreateOpts::new();
                let res = maybe_await!(ok_client().create("fake-id", "fake-bundle", Some(&opts)))
                    .expect("/bin/true failed");
                assert_ne!(res.pid, 0);
                assert!(res.status.success());
                assert!(res.output.is_empty());

                assert_command_failed(maybe_await!(fail_client().create(
                    "fake-id",
                    "fake-bundle",
                    Some(&opts)
                )));
            }

            #[$test]
            $($maybe_async)? fn test_run() {
                let opts = CreateOpts::new();
                let res = maybe_await!(ok_client().run("fake-id", "fake-bundle", Some(&opts)))
                    .expect("/bin/true failed");
                assert_ne!(res.pid, 0);
                assert!(res.status.success());
                assert!(res.output.is_empty());

                assert_command_failed(maybe_await!(fail_client().run(
                    "fake-id",
                    "fake-bundle",
                    Some(&opts)
                )));
            }

            #[$test]
            $($maybe_async)? fn test_start() {
                let res = maybe_await!(ok_client().start("fake-id")).expect("/bin/true failed");
                assert!(res.status.success());

                assert_command_failed(maybe_await!(fail_client().start("fake-id")));
            }

            #[$test]
            $($maybe_async)? fn test_exec() {
                let opts = ExecOpts::new();
                let proc = dummy_process();
                maybe_await!(ok_client().exec("fake-id", &proc, Some(&opts)))
                    .expect("/bin/true failed");

                assert_command_failed(maybe_await!(fail_client().exec(
                    "fake-id",
                    &proc,
                    Some(&opts)
                )));
            }

            #[$test]
            $($maybe_async)? fn test_delete() {
                let opts = DeleteOpts::new();
                maybe_await!(ok_client().delete("fake-id", Some(&opts))).expect("/bin/true failed");

                assert_command_failed(maybe_await!(fail_client().delete("fake-id", Some(&opts))));
            }

            /// The Io driver decides whether the child's output reaches us at all.
            #[$test]
            $($maybe_async)? fn test_output() {
                let mut opts = CreateOpts::new();
                opts.io = Some(Arc::new(InheritedStdIo::new().unwrap()));
                let res = maybe_await!(echo_client().create("fake-id", "fake-bundle", Some(&opts)))
                    .expect("/bin/echo failed");
                assert_ne!(res.pid, 0);
                assert!(res.status.success());
                assert!(res.output.is_empty(), "inherited stdio should capture nothing");

                let mut opts = CreateOpts::new();
                opts.io = Some(Arc::new(PipedStdIo::new().unwrap()));
                let res = maybe_await!(echo_client().create("fake-id", "fake-bundle", Some(&opts)))
                    .expect("/bin/echo failed");
                assert_ne!(res.pid, 0);
                assert!(res.status.success());
                assert!(!res.output.is_empty(), "piped stdio should capture the echo");
            }
        };
    }

    #[cfg(feature = "async")]
    runc_tests!(tokio::test, async);
    #[cfg(not(feature = "async"))]
    runc_tests!(test);
}
