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

use client::{
    events::{ContainerCreate, ContainerDelete},
    services::v1::{events_client::EventsClient, SubscribeRequest},
};
use containerd_client as client;

/// Make sure you run containerd before running this example.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let channel = client::connect("/run/containerd/containerd.sock")
        .await
        .expect("Connect Failed");

    let mut client = EventsClient::new(channel.clone());

    let request = SubscribeRequest::default();
    let mut response = client
        .subscribe(request)
        .await
        .expect("failed to subscribe to events")
        .into_inner();

    loop {
        match response.message().await {
            Ok(event) => {
                if let Some(event) = event {
                    match event.topic.as_str() {
                        "/containers/create" => {
                            if let Some(payload) = event.event {
                                let payload: ContainerCreate = payload
                                    .to_msg()
                                    .expect("failed to parse ContainerCreate payload");

                                println!(
                                    "container created: id={} image={}",
                                    payload.id, payload.image
                                );
                            }
                        }
                        "/containers/delete" => {
                            if let Some(payload) = event.event {
                                let payload: ContainerDelete = payload
                                    .to_msg()
                                    .expect("failed to parse ContainerDelete payload");

                                println!("container deleted: id={}", payload.id);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                eprintln!("error while streaming events: {:?}", e);
                break;
            }
        }
    }
}
