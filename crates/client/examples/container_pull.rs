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

use std::env::consts;

use client::{
    services::v1::{transfer_client::TransferClient, TransferOptions, TransferRequest},
    to_any,
    types::{
        transfer::{ImageStore, OciRegistry, UnpackConfiguration},
        Platform,
    },
    with_namespace,
};
use containerd_client as client;
use tonic::Request;

const IMAGE: &str = "docker.io/library/alpine:latest";
const NAMESPACE: &str = "default";

/// Make sure you run containerd before running this example.
/// NOTE: to run this example, you must prepare a rootfs.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let arch = match consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => consts::ARCH,
    };

    let channel = client::connect("/run/containerd/containerd.sock")
        .await
        .expect("Connect Failed");
    let mut client = TransferClient::new(channel.clone());

    // Create the source (OCIRegistry)
    let source = OciRegistry {
        reference: IMAGE.to_string(),
        resolver: Default::default(),
    };

    let platform = Platform {
        os: "linux".to_string(),
        architecture: arch.to_string(),
        variant: "".to_string(),
        os_version: "".to_string(),
    };

    // Create the destination (ImageStore)
    let destination = ImageStore {
        name: IMAGE.to_string(),
        platforms: vec![platform.clone()],
        unpacks: vec![UnpackConfiguration {
            platform: Some(platform),
            ..Default::default()
        }],
        ..Default::default()
    };

    let anys = to_any(&source);
    let anyd = to_any(&destination);

    println!("Pulling image for linux/{} from source: {:?}", arch, source);

    // Create the transfer request
    let request = TransferRequest {
        source: Some(anys),
        destination: Some(anyd),
        options: Some(TransferOptions {
            ..Default::default()
        }),
    };
    // Execute the transfer (pull)
    client
        .transfer(with_namespace!(request, NAMESPACE))
        .await
        .expect("unable to transfer image");
}
