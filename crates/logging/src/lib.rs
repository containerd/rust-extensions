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

//! Helper library to implement custom shim loggers for containerd.
//!
//! # Runtime
//! containerd shims may support pluggable logging via stdio URIs. Binary logging has the ability to
//! forward a container's STDIO to an external binary for consumption.
//!
//! This crates replicates APIs provided by Go [version](https://github.com/containerd/containerd/blob/main/runtime/v2/README.md#logging).
//!

use std::env;
use std::fmt;
use std::fs;
use std::os::unix::io::FromRawFd;
use std::process;

/// Logging binary configuration received from containerd.
#[derive(Debug)]
pub struct Config {
    /// Container id.
    pub id: String,
    /// Container namespace.
    pub namespace: String,
    /// Stdout to forward logs from.
    pub stdout: fs::File,
    /// Stderr to forward logs from.
    pub stderr: fs::File,
}

impl Config {
    /// Creates a new configuration object.
    ///
    /// It'll query environment provided by containerd to fill up [Config] structure fields.
    ///
    /// # Panics
    /// Function call will panic if the environment is incorrect (note that this should be happen
    /// if launched from containerd).
    ///
    fn new() -> Config {
        let id = match env::var("CONTAINER_ID") {
            Ok(id) => id,
            Err(_) => handle_err("CONTAINER_ID env not found"),
        };

        let namespace = match env::var("CONTAINER_NAMESPACE") {
            Ok(ns) => ns,
            Err(_) => handle_err("CONTAINER_NAMESPACE env not found"),
        };

        let stdout = unsafe { fs::File::from_raw_fd(3) };
        let stderr = unsafe { fs::File::from_raw_fd(4) };

        Config {
            id,
            namespace,
            stdout,
            stderr,
        }
    }
}

/// Signal file wrapper.
/// containerd uses a file with fd 5 as a signaling mechanism between the daemon and logger process.
/// This is a wrapper for convenience.
///
/// See [logging_unix.go] for details.
///
/// [logging_unix.go]: https://github.com/containerd/containerd/blob/dbef1d56d7ebc05bc4553d72c419ed5ce025b05d/runtime/v2/logging/logging_unix.go#L44
struct Ready(fs::File);

impl Ready {
    fn new() -> Ready {
        Ready(unsafe { fs::File::from_raw_fd(5) })
    }

    /// Signal that we are ready and setup for the container to be started.
    fn signal(self) {
        drop(self.0)
    }
}

/// Driver is a trait to be implemented by v2 logging binaries.
///
/// This trait is Rusty alternative to Go's `LoggerFunc`.
///
/// # Example
///
/// ```rust
/// use containerd_shim_logging::{Config, Driver};
///
/// struct Logger;
///
/// impl Driver for Logger {
///     type Error = ();
///
///     // Launch logger threads here.
///     fn new(config: Config) -> Result<Self, Self::Error> {
///         Ok(Logger {})
///     }
///
///     // Wait for threads to finish.
///     // In this example `Logger` will finish immediately.
///     fn wait(self) -> Result<(), Self::Error> {
///         Ok(())
///     }
/// }
/// ```
pub trait Driver: Sized {
    /// The error type to be returned from driver routines if something goes wrong.
    type Error: fmt::Debug;

    /// Create and run a new binary logger from the provided [Config].
    ///
    /// Implementations are expected to start the logger driver (typically by spawning threads).
    /// Once returned, the crate will signal containerd that we're ready to log.
    fn new(config: Config) -> Result<Self, Self::Error>;

    /// Wait for the driver to finish.
    ///
    /// Once returned from this function, the binary logger process will shutdown.
    fn wait(self) -> Result<(), Self::Error>;
}

/// Entry point to run the logging driver.
///
/// Typically `run` must be called from the `main` function to launch the driver.
pub fn run<D: Driver>() {
    let config = Config::new();
    let ready = Ready::new();

    // Initialize log driver
    let logger = match D::new(config) {
        Ok(driver) => driver,
        Err(err) => handle_err(err),
    };

    // Signal ready to pump log data
    ready.signal();

    // Run and block until exit
    if let Err(err) = logger.wait() {
        handle_err(err)
    } else {
        process::exit(0);
    }
}

#[inline]
fn handle_err(err: impl fmt::Debug) -> ! {
    eprintln!("{:?}", err);
    process::exit(1);
}
