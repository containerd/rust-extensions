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

use std::collections::HashMap;
use std::env;

use futures::TryFutureExt;
use log::info;
use tokio::net::UnixListener;

use containerd_snapshots as snapshots;
use containerd_snapshots::{api, Info, Usage};

use snapshots::tonic::transport::Server;

#[derive(Default)]
struct Example;

#[snapshots::tonic::async_trait]
impl snapshots::Snapshotter for Example {
    type Error = String;

    async fn stat(&self, key: String) -> Result<Info, Self::Error> {
        info!("Stat: {}", key);
        Ok(Info::default())
    }

    async fn update(
        &self,
        info: Info,
        fieldpaths: Option<Vec<String>>,
    ) -> Result<Info, Self::Error> {
        info!("Update: info={:?}, fieldpaths={:?}", info, fieldpaths);
        Ok(Info::default())
    }

    async fn usage(&self, key: String) -> Result<Usage, Self::Error> {
        info!("Usage: {}", key);
        Ok(Usage::default())
    }

    async fn mounts(&self, key: String) -> Result<Vec<api::types::Mount>, Self::Error> {
        info!("Mounts: {}", key);
        Ok(Vec::new())
    }

    async fn prepare(
        &self,
        key: String,
        parent: String,
        labels: HashMap<String, String>,
    ) -> Result<Vec<api::types::Mount>, Self::Error> {
        info!(
            "Prepare: key={}, parent={}, labels={:?}",
            key, parent, labels
        );
        Ok(Vec::new())
    }

    async fn view(
        &self,
        key: String,
        parent: String,
        labels: HashMap<String, String>,
    ) -> Result<Vec<api::types::Mount>, Self::Error> {
        info!("View: key={}, parent={}, labels={:?}", key, parent, labels);
        Ok(Vec::new())
    }

    async fn commit(
        &self,
        name: String,
        key: String,
        labels: HashMap<String, String>,
    ) -> Result<(), Self::Error> {
        info!("Commit: name={}, key={}, labels={:?}", name, key, labels);
        Ok(())
    }

    async fn remove(&self, key: String) -> Result<(), Self::Error> {
        info!("Remove: {}", key);
        Ok(())
    }
}

#[cfg(unix)]
#[tokio::main(flavor = "current_thread")]
async fn main() {
    simple_logger::SimpleLogger::new()
        .init()
        .expect("Failed to initialize logger");

    let args = env::args().collect::<Vec<_>>();

    let socket_path = args
        .get(1)
        .ok_or("First argument must be socket path")
        .unwrap();

    let example = Example::default();

    let incoming = {
        let uds = UnixListener::bind(socket_path).expect("Failed to bind listener");

        async_stream::stream! {
            loop {
                let item = uds.accept().map_ok(|(st, _)| unix::UnixStream(st)).await;
                yield item;
            }
        }
    };

    Server::builder()
        .add_service(snapshots::server(example))
        .serve_with_incoming(incoming)
        .await
        .expect("Serve failed");
}

// Copy-pasted from https://github.com/hyperium/tonic/blob/master/examples/src/uds/server.rs#L69
#[cfg(unix)]
mod unix {
    use std::{
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    };

    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tonic::transport::server::Connected;

    #[derive(Debug)]
    pub struct UnixStream(pub tokio::net::UnixStream);

    impl Connected for UnixStream {
        type ConnectInfo = UdsConnectInfo;

        fn connect_info(&self) -> Self::ConnectInfo {
            UdsConnectInfo {
                peer_addr: self.0.peer_addr().ok().map(Arc::new),
                peer_cred: self.0.peer_cred().ok(),
            }
        }
    }

    #[derive(Clone, Debug)]
    pub struct UdsConnectInfo {
        pub peer_addr: Option<Arc<tokio::net::unix::SocketAddr>>,
        pub peer_cred: Option<tokio::net::unix::UCred>,
    }

    impl AsyncRead for UnixStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for UnixStream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }
}

#[cfg(not(unix))]
fn main() {
    panic!("The snapshotter example only works on unix systems!");
}
