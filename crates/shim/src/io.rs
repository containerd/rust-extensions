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

#[derive(Clone, Debug)]
pub struct Stdio {
    pub stdin: String,
    pub stdout: String,
    pub stderr: String,
    pub terminal: bool,
}

impl Stdio {
    pub fn new(stdin: &str, stdout: &str, stderr: &str, terminal: bool) -> Self {
        Self {
            stdin: stdin.to_string(),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            terminal,
        }
    }

    pub fn is_null(&self) -> bool {
        self.stdin.is_empty() && self.stdout.is_empty() && self.stderr.is_empty()
    }
}
