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

use std::io;

use chrono::{DateTime, Utc};

use super::config::{ExecConfig, StdioConfig};
use super::state::ProcessState;

pub trait InitState {
    fn start(&mut self) -> io::Result<()>;
    fn delete(&mut self) -> io::Result<()>;
    fn pause(&mut self) -> io::Result<()>;
    fn resume(&mut self) -> io::Result<()>;
    fn update(&mut self, resource_config: Option<&dyn std::any::Any>) -> io::Result<()>;
    // FIXME: suspended for difficulties
    // fn checkpoint(&self) -> io::Result<()>;
    fn exec(&self, config: ExecConfig) -> io::Result<()>;
    fn kill(&mut self, sig: u32, all: bool) -> io::Result<()>;
    fn set_exited(&mut self, status: isize);
    fn state(&self) -> io::Result<ProcessState>;
}

pub trait Process {
    fn id(&self) -> String;
    fn pid(&self) -> isize;
    fn exit_status(&self) -> isize;
    fn exited_at(&self) -> Option<DateTime<Utc>>;
    // FIXME: suspended for difficulties
    // fn stdin(&self) -> Option<Fifo>;
    fn stdio(&self) -> StdioConfig;
    fn wait(&mut self) -> io::Result<()>;
    // FIXME: suspended for difficulties
    // fn resize(&self) -> io::Result<()>;
    fn start(&mut self) -> io::Result<()>;
    fn delete(&mut self) -> io::Result<()>;
    fn kill(&mut self, sig: u32, all: bool) -> io::Result<()>;
    fn set_exited(&mut self, status: isize);
    fn state(&self) -> io::Result<ProcessState>;
}

pub trait ContainerProcess: InitState + Process {}
