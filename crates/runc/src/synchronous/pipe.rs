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

use os_pipe::{pipe, PipeReader, PipeWriter};

#[derive(Debug)]
pub struct Pipe {
    pub rd: PipeReader,
    pub wr: PipeWriter,
}

impl Pipe {
    pub fn new() -> std::io::Result<Self> {
        let (rd, wr) = pipe()?;
        Ok(Self { rd, wr })
    }
}
