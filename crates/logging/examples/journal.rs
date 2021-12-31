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

use std::fs;
use std::io;
use std::io::BufRead;
use std::thread;

use containerd_shim_logging as logging;

use logging::{Config, Driver};

fn pump(reader: fs::File) {
    io::BufReader::new(reader)
        .lines()
        .filter_map(|line| line.ok())
        .for_each(|_str| {
            // Write log string to destination here.
            // For instance with journald:
            //  systemd::journal::print(0, &str);
        });
}

struct Journal {
    stdout_handle: thread::JoinHandle<()>,
    stderr_handle: thread::JoinHandle<()>,
}

impl Driver for Journal {
    type Error = String;

    fn new(config: Config) -> Result<Self, Self::Error> {
        let stdout = config.stdout;
        let stderr = config.stderr;

        Ok(Journal {
            stdout_handle: thread::spawn(|| pump(stdout)),
            stderr_handle: thread::spawn(|| pump(stderr)),
        })
    }

    fn wait(self) -> Result<(), Self::Error> {
        self.stdout_handle
            .join()
            .map_err(|err| format!("{:?}", err))?;
        self.stderr_handle
            .join()
            .map_err(|err| format!("{:?}", err))?;
        Ok(())
    }
}

fn main() {
    logging::run::<Journal>()
}
