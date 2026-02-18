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

use std::io::{PipeReader, PipeWriter};
use std::sync::Mutex;

#[derive(Debug)]
pub struct Pipe {
    pub rd: PipeReader,
    wr: Mutex<Option<PipeWriter>>,
}

impl Pipe {
    pub fn new() -> std::io::Result<Self> {
        let (rd, wr) = std::io::pipe()?;
        Ok(Self {
            rd,
            wr: Mutex::new(Some(wr)),
        })
    }

    /// Clone the write end. Returns `None` if closed.
    pub fn try_clone_wr(&self) -> Option<PipeWriter> {
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
}
