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

use containerd_client as client;

use client::services::v1::container::Runtime;
use client::services::v1::containers_client::ContainersClient;
use client::services::v1::Container;
use client::services::v1::{CreateContainerRequest, DeleteContainerRequest};

use client::services::v1::tasks_client::TasksClient;
use client::services::v1::{CreateTaskRequest, DeleteTaskRequest, StartRequest, WaitRequest};

use prost_types::Any;

use tonic::Request;

use std::fs;
use std::fs::File;

use client::with_namespace;

const CID: &str = "abc123";
const NAMESPACE: &str = "default";

/// Make sure you run containerd before running this example.
/// NOTE: to run this example, you must prepare a rootfs.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let channel = client::connect("/run/containerd/containerd.sock")
        .await
        .expect("Connect Failed");

    let mut client = ContainersClient::new(channel.clone());

    let rootfs = "/tmp/busybox/bundle/rootfs";
    // the container will run with command `echo $output`
    let output = "hello rust client";

    let spec = include_str!("container_spec.json");
    let spec = spec
        .to_string()
        .replace("$ROOTFS", rootfs)
        .replace("$OUTPUT", output);

    let spec = Any {
        type_url: "types.containerd.io/opencontainers/runtime-spec/1/Spec".to_string(),
        value: spec.into_bytes(),
    };

    let container = Container {
        id: CID.to_string(),
        image: "docker.io/library/alpine:latest".to_string(),
        runtime: Some(Runtime {
            name: "io.containerd.runc.v2".to_string(),
            options: None,
        }),
        spec: Some(spec),
        ..Default::default()
    };

    let req = CreateContainerRequest {
        container: Some(container),
    };
    let req = with_namespace!(req, NAMESPACE);

    let _resp = client
        .create(req)
        .await
        .expect("Failed to create container");

    println!("Container: {:?} created", CID);

    // create temp dir for stdin/stdout/stderr
    let tmp = std::env::temp_dir().join("containerd-client-test");
    fs::create_dir_all(&tmp).expect("Failed to create temp directory");
    let stdin = tmp.join("stdin");
    let stdout = tmp.join("stdout");
    let stderr = tmp.join("stderr");
    File::create(&stdin).expect("Failed to create stdin");
    File::create(&stdout).expect("Failed to create stdout");
    File::create(&stderr).expect("Failed to create stderr");

    // creat and start task
    let mut client = TasksClient::new(channel.clone());

    let req = CreateTaskRequest {
        container_id: CID.to_string(),
        stdin: stdin.to_str().unwrap().to_string(),
        stdout: stdout.to_str().unwrap().to_string(),
        stderr: stderr.to_str().unwrap().to_string(),
        ..Default::default()
    };
    let req = with_namespace!(req, NAMESPACE);

    let _resp = client.create(req).await.expect("Failed to create task");

    println!("Task: {:?} created", CID);

    let req = StartRequest {
        container_id: CID.to_string(),
        ..Default::default()
    };
    let req = with_namespace!(req, NAMESPACE);

    let _resp = client.start(req).await.expect("Failed to start task");

    println!("Task: {:?} started", CID);

    // wait task
    let req = WaitRequest {
        container_id: CID.to_string(),
        ..Default::default()
    };
    let req = with_namespace!(req, NAMESPACE);

    let _resp = client.wait(req).await.expect("Failed to wait task");

    println!("Task: {:?} stopped", CID);

    // delete task
    let req = DeleteTaskRequest {
        container_id: CID.to_string(),
    };
    let req = with_namespace!(req, NAMESPACE);

    let _resp = client.delete(req).await.expect("Failed to delete task");

    println!("Task: {:?} deleted", CID);

    // delete container
    let mut client = ContainersClient::new(channel);

    let req = DeleteContainerRequest {
        id: CID.to_string(),
    };
    let req = with_namespace!(req, NAMESPACE);

    let _resp = client
        .delete(req)
        .await
        .expect("Failed to delete container");

    println!("Container: {:?} deleted", CID);

    // test container output
    let actual_stdout = fs::read_to_string(stdout).expect("read stdout actual");
    assert_eq!(actual_stdout.strip_suffix('\n').unwrap(), output);

    // clear stdin/stdout/stderr
    let _ = fs::remove_dir_all(tmp);
}
