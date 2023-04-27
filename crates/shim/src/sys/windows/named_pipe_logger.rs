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
    io::{self, Write},
    sync::{Arc, Mutex},
    thread,
};

use log::{Metadata, Record};
use mio::{windows::NamedPipe, Events, Interest, Poll, Token};

pub struct NamedPipeLogger {
    current_connection: Arc<Mutex<NamedPipe>>,
}

impl NamedPipeLogger {
    pub fn new(namespace: &str, id: &str) -> Result<NamedPipeLogger, io::Error> {
        let pipe_name = format!("\\\\.\\pipe\\containerd-shim-{}-{}-log", namespace, id);
        let mut pipe_server = NamedPipe::new(pipe_name).unwrap();
        let mut poll = Poll::new().unwrap();
        poll.registry()
            .register(
                &mut pipe_server,
                Token(0),
                Interest::READABLE | Interest::WRITABLE,
            )
            .unwrap();

        let current_connection = Arc::new(Mutex::new(pipe_server));
        let server_connection = current_connection.clone();
        let logger = NamedPipeLogger { current_connection };

        thread::spawn(move || {
            let mut events = Events::with_capacity(128);
            loop {
                poll.poll(&mut events, None).unwrap();

                for event in events.iter() {
                    if event.is_writable() {
                        match server_connection.lock().unwrap().connect() {
                            Ok(()) => {}
                            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {
                                // this would block just keep processing
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                // this would block just keep processing
                            }
                            Err(e) => {
                                panic!("Error connecting to client: {}", e);
                            }
                        };
                    }
                    if event.is_readable() {
                        server_connection.lock().unwrap().disconnect().unwrap();
                    }
                }
            }
        });

        Ok(logger)
    }
}

impl log::Log for NamedPipeLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let message = format!("[{}] {}\n", record.level(), record.args());

            match self
                .current_connection
                .lock()
                .unwrap()
                .write(message.as_bytes())
            {
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {
                    // this would block just keep processing
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // this would block just keep processing
                }
                Err(e) if e.raw_os_error() == Some(536) => {
                    // no client connected
                }
                Err(e) if e.raw_os_error() == Some(232) => {
                    // client was connected but is in process of shutting down
                }
                Err(e) => {
                    panic!("Error writing to client: {}", e)
                }
            }
        }
    }

    fn flush(&self) {
        _ = self.current_connection.lock().unwrap().flush();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::OpenOptions,
        io::Read,
        os::windows::{
            fs::OpenOptionsExt,
            io::{FromRawHandle, IntoRawHandle},
            prelude::AsRawHandle,
        },
        time::Duration,
    };

    use log::{Log, Record};
    use mio::{windows::NamedPipe, Events, Interest, Poll, Token};
    use windows_sys::Win32::{
        Foundation::ERROR_PIPE_NOT_CONNECTED, Storage::FileSystem::FILE_FLAG_OVERLAPPED,
    };

    use super::*;

    #[test]
    fn test_namedpipe_log_can_write_before_client_connected() {
        let ns = "test".to_string();
        let id = "notconnected".to_string();
        let logger = NamedPipeLogger::new(&ns, &id).unwrap();

        // test can write before a reader is connected (should succeed but the messages will be dropped)
        log::set_max_level(log::LevelFilter::Info);
        let record = Record::builder()
            .level(log::Level::Info)
            .line(Some(1))
            .file(Some("sample file"))
            .args(format_args!("hello"))
            .build();
        logger.log(&record);
        logger.flush();
    }

    #[test]
    fn test_namedpipe_log() {
        use std::fs::File;

        let ns = "test".to_string();
        let id = "clients".to_string();
        let pipe_name = format!("\\\\.\\pipe\\containerd-shim-{}-{}-log", ns, id);

        let logger = NamedPipeLogger::new(&ns, &id).unwrap();
        let mut client = create_client(pipe_name.as_str());

        log::set_max_level(log::LevelFilter::Info);
        let record = Record::builder()
            .level(log::Level::Info)
            .line(Some(1))
            .file(Some("sample file"))
            .args(format_args!("hello"))
            .build();
        logger.log(&record);
        logger.flush();

        let buf = read_message(&mut client);
        assert_eq!("[INFO] hello", std::str::from_utf8(&buf).unwrap());

        // test that we can reconnect after a reader disconnects
        // we need to get the raw handle and drop that as well to force full disconnect
        // and give a few milliseconds for the disconnect to happen
        println!("dropping client");
        let handle = client.as_raw_handle();
        drop(client);
        let f = unsafe { File::from_raw_handle(handle) };
        drop(f);
        std::thread::sleep(Duration::from_millis(100));

        let mut client2 = create_client(pipe_name.as_str());
        logger.log(&record);
        logger.flush();

        read_message(&mut client2);
    }

    fn read_message(client: &mut NamedPipe) -> [u8; 12] {
        let mut poll = Poll::new().unwrap();
        poll.registry()
            .register(client, Token(1), Interest::READABLE)
            .unwrap();
        let mut events = Events::with_capacity(128);
        let mut buf = [0; 12];
        loop {
            poll.poll(&mut events, Some(Duration::from_millis(10)))
                .unwrap();
            match client.read(&mut buf) {
                Ok(0) => {
                    panic!("Read no bytes from pipe")
                }
                Ok(_) => {
                    break;
                }
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_NOT_CONNECTED as i32) => {
                    panic!("not connected to the pipe");
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    continue;
                }
                Err(e) => panic!("Error reading from pipe: {}", e),
            }
        }
        buf
    }

    fn create_client(pipe_name: &str) -> mio::windows::NamedPipe {
        let mut opts = OpenOptions::new();
        opts.read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OVERLAPPED);
        let file = opts.open(pipe_name).unwrap();

        unsafe { NamedPipe::from_raw_handle(file.into_raw_handle()) }
    }
}
