use std::borrow::BorrowMut;
use std::fs::{File, OpenOptions};
use std::io;
use std::io::Write;
use std::sync::Mutex;

use log::{Metadata, Record};
use thiserror::Error;

pub struct FifoLogger {
    file: Mutex<File>,
}

impl FifoLogger {
    pub fn new() -> Result<FifoLogger, io::Error> {
        let f = OpenOptions::new()
            .write(true)
            .read(false)
            .create(false)
            .open("log")?;

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
            writeln!(guard.borrow_mut(), "[{}] {}", record.level(), record.args())
                .expect("Failed to write log fifo file");
        }
    }

    fn flush(&self) {
        self.file
            .lock()
            .unwrap()
            .sync_all()
            .expect("Failed to sync log data");
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("Failed to setup logger: {0}")]
    Setup(#[from] log::SetLoggerError),
}

pub fn init(debug: bool) -> Result<(), Error> {
    let logger = FifoLogger::new().map_err(Error::Io)?;
    log::set_boxed_logger(Box::new(logger)).map_err(Error::Setup)?;

    log::set_max_level(if debug {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });

    Ok(())
}
