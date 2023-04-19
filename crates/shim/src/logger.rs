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

use std::{
    borrow::BorrowMut,
    fs::{File, OpenOptions},
    io,
    io::Write,
    path::Path,
    sync::Mutex,
};

use log::{Metadata, Record};

use crate::error::Error;
#[cfg(windows)]
use crate::sys::windows::NamedPipeLogger;

pub struct FifoLogger {
    file: Mutex<File>,
}

impl FifoLogger {
    #[allow(dead_code)]
    pub fn new() -> Result<FifoLogger, io::Error> {
        Self::with_path("log")
    }

    #[allow(dead_code)]
    pub fn with_path<P: AsRef<Path>>(path: P) -> Result<FifoLogger, io::Error> {
        let f = OpenOptions::new()
            .write(true)
            .read(false)
            .create(false)
            .open(path)?;

        Ok(FifoLogger {
            file: Mutex::new(f),
        })
    }
}

impl log::Log for FifoLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let mut guard = self.file.lock().unwrap();
            // The logger server may have temporarily shutdown, ignore the error instead of panic.
            //
            // Manual for pipe/FIFO: https://man7.org/linux/man-pages/man7/pipe.7.html
            // If all file descriptors referring to the read end of a pipe have been closed, then
            // a write(2) will cause a SIGPIPE signal to be generated for the calling process.
            // If the calling process is ignoring this signal, then write(2) fails with the error
            // EPIPE.
            let _ = writeln!(guard.borrow_mut(), "[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {
        // The logger server may have temporarily shutdown, ignore the error instead of panic.
        let _ = self.file.lock().unwrap().sync_all();
    }
}

#[cfg(unix)]
pub fn init(debug: bool) -> Result<(), Error> {
    let logger = FifoLogger::new().map_err(io_error!(e, "failed to init logger"))?;

    let boxed_logger = Box::new(logger);
    configure_logger(boxed_logger, debug)
}

#[cfg(windows)]
// Containerd on windows expects the log to be a named pipe in the format of \\.\pipe\containerd-<namespace>-<id>-log
// There is an assumption that there is always only one client connected which is containerd.
// If there is a restart of containerd then logs during that time period will be lost.
//
// https://github.com/containerd/containerd/blob/v1.7.0/runtime/v2/shim_windows.go#L77
// https://github.com/microsoft/hcsshim/blob/5871d0c4436f131c377655a3eb09fc9b5065f11d/cmd/containerd-shim-runhcs-v1/serve.go#L132-L137
pub fn init(debug: bool, namespace: &String, id: &String) -> Result<(), Error> {
    let logger =
        NamedPipeLogger::new(namespace, id).map_err(io_error!(e, "failed to init logger"))?;

    let boxed_logger = Box::new(logger);
    configure_logger(boxed_logger, debug)
}

fn configure_logger(logger: Box<dyn log::Log>, debug: bool) -> Result<(), Error> {
    let level = if debug {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };

    log::set_boxed_logger(logger)?;
    log::set_max_level(level);
    Ok(())
}

#[cfg(test)]
mod tests {
    use log::{Log, Record};

    use super::*;

    #[test]
    fn test_fifo_log() {
        #[cfg(unix)]
        use nix::{sys::stat, unistd};

        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().to_str().unwrap().to_owned() + "/log";

        #[cfg(unix)]
        unistd::mkfifo(Path::new(&path), stat::Mode::S_IRWXU).unwrap();

        #[cfg(windows)]
        File::create(path.clone()).unwrap();

        let path1 = path.clone();
        let thread = std::thread::spawn(move || {
            let _fifo = OpenOptions::new()
                .write(false)
                .read(true)
                .create(false)
                .open(path1)
                .unwrap();
        });

        let logger = FifoLogger::with_path(&path).unwrap();
        //log::set_boxed_logger(Box::new(logger)).map_err(Error::Setup)?;
        log::set_max_level(log::LevelFilter::Info);
        thread.join().unwrap();

        let record = Record::builder()
            .level(log::Level::Error)
            .line(Some(1))
            .file(Some("sample file"))
            .build();
        logger.log(&record);
        logger.flush();
    }
}
