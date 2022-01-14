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

#![allow(unused)]

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use containerd_shim_protos as protos;

use protos::shim::{
    shim::{
        DeleteRequest, KillRequest, StartRequest,
    },
};

use chrono::{DateTime, Utc};
use nix::errno::Errno;
use nix::sys::stat;
use nix::unistd;
use protobuf::Message;
use sys_mount::UnmountFlags;

use crate::options::oci::Options;
use crate::process::{
    config::{CreateConfig, MountConfig},
    init::InitProcess,
};
use crate::utils;

// for debug

const OPTIONS_FILENAME: &str = "options.json";

#[derive(Debug)]
/// Struct for managing runc containers.
pub struct Container {
    mu: Arc<Mutex<()>>,
    id: String,
    bundle: String,
    // FIXME: cgroup settings
    // cgroup: impl protos::api:: ,
    /// This container's process itself. (e.g. init process)
    process_self: InitProcess,
    /// processes running inside this container.
    processes: HashMap<String, InitProcess>,
}

impl Container {
    /// When this struct is created, container is ready to create.
    /// That means, mounting rootfs is done etc.
    pub fn new(req: protos::shim::shim::CreateTaskRequest) -> io::Result<Self> {
        // FIXME
        let namespace = "default".to_string();

        let opts = if req.options.is_some() && req.options.as_ref().unwrap().get_type_url() != "" {
            // FIXME: option should be unmarshaled
            // https://github.com/containerd/containerd/blob/main/runtime/v2/runc/container.go#L52
            // let v = unmarshal_any(req.options);
            // v.options.clone();
            Options::default()
        } else {
            Options::default()
        };

        let mut mounts = Vec::new();
        for mnt in &req.rootfs {
            mounts.push(MountConfig::from_proto_mount(mnt.clone()));
        }

        let rootfs = if mounts.len() > 0 {
            let path = Path::new(&req.bundle).join("rootfs");
            match unistd::mkdir(&path, stat::Mode::from_bits_truncate(0o711)) {
                Ok(_) | Err(Errno::EEXIST) => {}
                Err(e) => return Err(io::Error::from(e)),
            };
            path
        } else {
            PathBuf::new()
        };

        let config = CreateConfig {
            id: req.id.clone(),
            bundle: req.bundle.clone(),
            runtime: opts.binary_name.clone(),
            rootfs: mounts.clone(),
            terminal: req.terminal,
            stdin: req.stdin.clone(),
            stdout: req.stdout.clone(),
            stderr: req.stderr.clone(),
            options: req.options.clone().into_option(),
        };

        // Write options to file, which will be removed when shim stops.
        match write_options(&req.bundle, &opts) {
            Ok(_) => {}
            Err(e) => {
                return Err(e);
            }
        }
        // For historical reason, we write binary name as well as the entire opts
        write_runtime(&req.bundle, &opts.binary_name)?;

        // split functionality in order to cleanup rootfs when error occurs after mount.
        Self::inner_new(&rootfs, req, namespace, opts, config, mounts).map_err(|e| {
            if let Err(_) = sys_mount::unmount(rootfs, UnmountFlags::empty()) {}
            e
        })
    }

    fn inner_new<R>(
        rootfs: R,
        req: protos::shim::shim::CreateTaskRequest,
        namespace: String,
        opts: Options,
        config: CreateConfig,
        mounts: Vec<MountConfig>,
    ) -> io::Result<Self>
    where
        R: AsRef<Path>,
    {
        for mnt in mounts {
            utils::mount(mnt, &rootfs)?;
        }
        let id = req.id.clone();
        let bundle = req.bundle.clone();
        let mut init = InitProcess::new(
            &bundle,
            Path::new(&bundle).join("work"),
            namespace,
            config.clone(),
            opts,
            rootfs,
        )?;
        // create the init process
        init.create(config)?;
        let pid = init.pid();

        if pid > 0 {
            // FIXME: setting config for cgroup
        }

        Ok(Container {
            mu: Arc::default(),
            id,
            bundle,
            process_self: init,
            processes: HashMap::new(),
        })
    }

    pub fn all(&self) {
        unimplemented!()
    }

    pub fn execd_processes(&self) {
        unimplemented!()
    }

    pub fn pid(&self) -> isize {
        let _m = self.mu.lock().unwrap();
        self.process_self.pid()
    }

    pub fn cgroup(&self) /* -> [] */
    {
        unimplemented!()
    }

    pub fn cgroup_set(&self) /* -> [] */
    {
        unimplemented!()
    }

    pub fn reserve_process(&self) {
        unimplemented!()
    }

    pub fn process_add(&self) /* -> [] */
    {
        unimplemented!()
    }

    pub fn process_remove(&mut self, id: &str) -> Option<InitProcess> {
        let _m = self.mu.lock().unwrap();
        self.processes.remove(id)
    }

    pub fn process<'a>(&'a self, id: &str) -> io::Result<&'a InitProcess> {
        let _m = self.mu.lock().unwrap();
        if id == "" || id == self.id {
            Ok(&self.process_self)
        } else {
            let p = self
                .processes
                .get(id)
                .ok_or_else(|| io::ErrorKind::NotFound)?;
            Ok(p)
        }
    }

    pub fn process_mut<'a>(&'a mut self, id: &str) -> io::Result<&'a mut InitProcess> {
        let _m = self.mu.lock().unwrap();
        if id == "" || id == self.id {
            Ok(&mut self.process_self)
        } else {
            let p = self
                .processes
                .get_mut(id)
                .ok_or_else(|| io::ErrorKind::NotFound)?;
            Ok(p)
        }
    }

    /// Start a container process and return its pid
    pub fn start(&mut self, req: &StartRequest) -> Result<isize, Box<dyn std::error::Error>> {
        let p = self.process_mut(&req.id)?;
        p.start()?;
        Ok(p.pid())
    }

    pub fn delete(
        &mut self,
        req: &DeleteRequest,
    ) -> io::Result<(isize, isize, Option<DateTime<Utc>>)> {
        {
            let p = self.process_mut(&req.exec_id)?;
            p.delete()?;
        }
        if req.exec_id != "" {
            let p = self
                .process_remove(&req.exec_id)
                .ok_or(std::io::ErrorKind::NotFound)?;
            Ok((p.pid(), p.exit_status(), p.exited_at()))
        } else {
            let ref p = self.process_self;
            Ok((p.pid(), p.exit_status(), p.exited_at()))
        }
    }

    pub fn exec(&self) -> Result<(), Box<dyn std::error::Error>> {
        unimplemented!()
    }

    pub fn pause(&self) -> Result<(), Box<dyn std::error::Error>> {
        unimplemented!()
    }

    pub fn resume(&self) -> Result<(), Box<dyn std::error::Error>> {
        unimplemented!()
    }

    pub fn resize_pty(&self) -> Result<(), Box<dyn std::error::Error>> {
        unimplemented!()
    }

    pub fn kill(&mut self, req: &KillRequest) -> io::Result<()> {
        let p = self.process_mut(&req.id)?;
        p.kill(req.signal, req.all)
    }

    pub fn close_io(&self) -> Result<(), Box<dyn std::error::Error>> {
        unimplemented!()
    }

    pub fn checkpoint(&self) -> Result<(), Box<dyn std::error::Error>> {
        unimplemented!()
    }

    pub fn update(&self) -> Result<(), Box<dyn std::error::Error>> {
        unimplemented!()
    }

    pub fn has_pid(&self) -> Result<(), Box<dyn std::error::Error>> {
        unimplemented!()
    }
}

/// reads the option information from the path.
/// When the file does not exist, returns [`None`] without an error.
pub fn read_options<P>(path: P) -> io::Result<Option<Options>>
where
    P: AsRef<Path>,
{
    let file_path = path.as_ref().join(OPTIONS_FILENAME);
    let f = match File::open(file_path) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };
    // NOTE: serde_json::from_reader is usually slower than from_str or from_slice
    // after read file contents into memory.
    let mut reader = BufReader::new(f);
    let msg = Message::parse_from_reader(&mut reader)?;
    Ok(Some(msg))
}

pub fn write_options<P>(path: P, opts: &Options) -> io::Result<()>
where
    P: AsRef<Path>,
{
    let file_path = path.as_ref().join(OPTIONS_FILENAME);
    let f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .mode(0o600)
        .open(&file_path)?;
    let mut writer = BufWriter::new(f);
    opts.write_to_writer(&mut writer)?;
    writer.flush()?;
    Ok(())
}

pub fn read_runtime<P>(path: P) -> Result<String, Box<dyn std::error::Error>>
where
    P: AsRef<Path>,
{
    let file_path = path.as_ref().join("runtime");
    let f = fs::OpenOptions::new().read(true).open(&file_path)?;
    let mut reader = BufReader::new(f);
    let mut buf = String::new();
    let mut res = String::new();
    while reader.read_line(&mut buf)? > 0 {
        res.push_str(&buf);
    }
    Ok(res)
}

pub fn write_runtime<P, R>(path: P, runtime: R) -> io::Result<()>
where
    P: AsRef<Path>,
    R: AsRef<str>,
{
    let file_path = path.as_ref().join("runtime");
    let f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .mode(0o600)
        .open(&file_path)?;
    let mut writer = BufWriter::new(f);
    writer.write_all(runtime.as_ref().as_bytes())?;
    Ok(())
}
