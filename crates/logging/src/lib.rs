//! Helper library to implement custom shim loggers for containerd.

use std::env;
use std::fmt;
use std::fs;
use std::os::unix::io::FromRawFd;
use std::process;

/// Logging binary configuration received from containerd.
pub struct Config {
    pub id: String,
    pub namespace: String,
    pub stdout: fs::File,
    pub stderr: fs::File,
}

impl Config {
    /// Creates a new configuration object. This will pull environment provided by containerd to fill
    /// up [Config] structure.
    ///
    /// `new` will panic if the environment is incorrect (not likely to happen if used properly).
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

/// Tiny wrapper around containerd's wait signal file.
/// See https://github.com/containerd/containerd/blob/dbef1d56d7ebc05bc4553d72c419ed5ce025b05d/runtime/v2/logging/logging_unix.go#L44
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
/// This trait is Rusty alternative to Go's `LoggerFunc`.
pub trait Driver: Sized {
    /// The error type to be returned from driver routines if something goes wrong.
    type Error: fmt::Debug;

    /// Create a new binary logger implementation from the provided `Config`.
    /// Once returned, the crate will signal that we're ready and
    /// setup for the container to be started
    fn new(config: Config) -> Result<Self, Self::Error>;

    /// Run the logging driver.
    /// The process will shutdown once exited.
    fn run(self) -> Result<(), Self::Error>;
}

/// Entry point to run the logging driver.
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
    if let Err(err) = logger.run() {
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
