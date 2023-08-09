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

use containerd_client::Client;

/// Make sure you run containerd before running this example.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let client = Client::from_path("/var/run/containerd/containerd.sock")
        .await
        .expect("Connect failed");

    let resp = client
        .version()
        .version(())
        .await
        .expect("Failed to query version");

    println!("Response: {:?}", resp.get_ref());
}
