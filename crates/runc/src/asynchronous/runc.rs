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

use std::{fmt::Debug, process::ExitStatus};

use async_trait::async_trait;

use crate::{error::Error, Command, Result};

#[async_trait]
pub trait Spawner: Debug {
    async fn execute(&self, cmd: Command) -> Result<(ExitStatus, u32, String, String)>;
}

#[derive(Debug)]
pub struct DefaultExecutor {}

#[async_trait]
impl Spawner for DefaultExecutor {
    async fn execute(&self, cmd: Command) -> Result<(ExitStatus, u32, String, String)> {
        let mut cmd = cmd;
        let child = cmd.spawn().map_err(Error::ProcessSpawnFailed)?;
        let pid = child.id().unwrap();
        let result = child
            .wait_with_output()
            .await
            .map_err(Error::InvalidCommand)?;
        let status = result.status;
        let stdout = String::from_utf8_lossy(&result.stdout).to_string();
        let stderr = String::from_utf8_lossy(&result.stderr).to_string();
        Ok((status, pid, stdout, stderr))
    }
}
