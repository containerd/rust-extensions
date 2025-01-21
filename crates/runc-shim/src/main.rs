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

use std::{env, io::Write};

use containerd_shim::{
    asynchronous::run,
    parse,
    protos::protobuf::{well_known_types::any::Any, Message},
    run_info,
};

mod cgroup_memory;
mod common;
mod console;
mod container;
mod io;
mod processes;
mod runc;
mod service;
mod task;

use service::Service;

fn parse_version() {
    let os_args: Vec<_> = env::args_os().collect();
    let flags = match parse(&os_args[1..]) {
        Ok(flags) => flags,
        Err(e) => {
            eprintln!("Error parsing arguments: {}", e);
            std::process::exit(1);
        }
    };
    if flags.version {
        println!("{}:", os_args[0].to_string_lossy());
        println!("  Version: {}", env!("CARGO_PKG_VERSION"));
        println!("  Revision: {}", env!("CARGO_GIT_HASH"));
        println!();

        std::process::exit(0);
    }
    if flags.info {
        let r = run_info();
        match r {
            Ok(rinfo) => {
                let mut info = Any::new();
                info.type_url = "io.containerd.runc.v2.Info".to_string();
                info.value = match rinfo.write_to_bytes() {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        eprintln!("Failed to write runtime info to bytes: {}", e);
                        std::process::exit(1);
                    }
                };
                std::io::stdout()
                    .write_all(info.write_to_bytes().unwrap().as_slice())
                    .expect("Failed to write to stdout");
            }
            Err(_) => {
                eprintln!("Failed to get runtime info");
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }
}

#[tokio::main]
async fn main() {
    parse_version();
    run::<Service>("io.containerd.runc.v2-rs", None).await;
}
