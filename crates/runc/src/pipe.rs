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

use std::{os::unix::io::OwnedFd, sync::Mutex};

/// A pipe carrying one of a process's stdio streams: the process gets one end
/// and the caller keeps the other.
///
/// The write end is tracked so that it can be closed, or given away, once the
/// process holds its copy; [`None`] then stands for a closed end.
#[derive(Debug)]
pub struct Pipe {
    pub rd: OwnedFd,
    wr: Mutex<Option<OwnedFd>>,
}

impl Pipe {
    pub fn new() -> std::io::Result<Self> {
        let (rd, wr) = std::io::pipe()?;
        Ok(Self {
            rd: OwnedFd::from(rd),
            wr: Mutex::new(Some(OwnedFd::from(wr))),
        })
    }

    /// Clone the write end. Returns `None` if closed.
    pub fn try_clone_wr(&self) -> Option<OwnedFd> {
        self.wr
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|w| w.try_clone().ok())
    }

    /// Close the write end by dropping it. No-op if already closed.
    pub fn close_wr(&self) {
        let _ = self.wr.lock().unwrap().take();
    }

    /// Take ownership of the write end. Returns `None` if already closed.
    pub fn take_wr(&self) -> Option<OwnedFd> {
        self.wr.lock().unwrap().take()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::File,
        io::{Read, Write},
    };

    use super::*;

    #[test]
    fn write_end_reaches_read_end() {
        let pipe = Pipe::new().unwrap();
        let mut wr = File::from(pipe.take_wr().unwrap());
        assert!(pipe.take_wr().is_none(), "the write end is given away once");
        wr.write_all(b"hello").unwrap();
        drop(wr);

        let mut read = String::new();
        File::from(pipe.rd).read_to_string(&mut read).unwrap();
        assert_eq!(read, "hello");
    }

    #[test]
    fn closing_the_write_end_ends_reads() {
        let pipe = Pipe::new().unwrap();
        File::from(pipe.try_clone_wr().unwrap())
            .write_all(b"x")
            .unwrap();
        pipe.close_wr();
        assert!(pipe.try_clone_wr().is_none());

        let mut read = String::new();
        File::from(pipe.rd).read_to_string(&mut read).unwrap();
        assert_eq!(read, "x");
    }
}
