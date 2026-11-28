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

//! Implements a client to publish events from the shim back to containerd.
//!
//! As in the Go shim publisher, a forward that fails because the connection to containerd is
//! gone (for example because containerd restarted) reconnects and resends once. An event that
//! still can't be sent, or whose forward timed out, is requeued on a background thread, which
//! retries it with growing delays and drops it after a few tries. A forward that timed out, or
//! whose connection closed before the reply, may already have reached containerd, so containerd
//! can receive an event more than once, and a requeued event can reach containerd after later
//! ones. Unlike in Go, an error status returned by containerd is not retried.

use std::{
    collections::VecDeque,
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use client::{
    protobuf::MessageDyn,
    shim::{event::Envelope, events},
    ttrpc::{self, context::Context},
    types::empty,
    Client, Events, EventsClient,
};
use containerd_shim_protos as client;
use log::{debug, error, warn};

#[cfg(unix)]
use crate::util::connect;
use crate::{
    error::Result,
    util::{convert_to_any, timestamp},
    Error,
};

#[cfg(windows)]
const RETRY_COUNT: i32 = 3;

/// Timeout of a forward request when the caller's context has none. The test value only keeps the
/// tests short.
const FORWARD_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(1)
} else {
    Duration::from_secs(5)
};
/// Maximum number of events waiting in the retry thread. The channel that hands events to it has
/// the same capacity, so up to twice as many events can be waiting in all.
const QUEUE_SIZE: usize = 1024;
/// Maximum number of retries of a requeued event.
const MAX_REQUEUE: u32 = 5;
/// Delay before the first retry of a requeued event. The n-th retry comes n times this delay after
/// the previous try, as in the Go shim. The test value only keeps the tests short.
const RETRY_DELAY: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(1)
};

/// Remote publisher connects to containerd's TTRPC endpoint to publish events from shim.
pub struct RemotePublisher {
    inner: Arc<Inner>,
    requeue: SyncSender<Item>,
}

struct Inner {
    address: String,
    /// The client, and how many times it was replaced.
    client: Mutex<(u64, EventsClient)>,
}

struct Item {
    ctx: Context,
    req: events::ForwardRequest,
    /// Number of times the event has been requeued.
    count: u32,
}

impl RemotePublisher {
    /// Connect to containerd's TTRPC endpoint.
    ///
    /// containerd uses `/run/containerd/containerd.sock.ttrpc` by default
    pub fn new(address: impl AsRef<str>) -> Result<RemotePublisher> {
        let client = Self::connect(address.as_ref())?;
        let inner = Arc::new(Inner {
            address: address.as_ref().to_owned(),
            client: Mutex::new((0, EventsClient::new(client))),
        });

        let (requeue, receiver) = mpsc::sync_channel(QUEUE_SIZE);
        let worker = inner.clone();
        thread::Builder::new()
            .name("shim-publisher".to_owned())
            .spawn(move || worker.process_queue(receiver))
            .map_err(io_error!(e, "spawn publisher retry thread"))?;

        Ok(RemotePublisher { inner, requeue })
    }

    fn connect(address: impl AsRef<str>) -> Result<Client> {
        #[cfg(unix)]
        {
            let fd = connect(address)?;
            // Client::new() takes ownership of the RawFd.
            Client::new(fd).map_err(|err| err.into())
        }

        #[cfg(windows)]
        {
            for i in 0..RETRY_COUNT {
                match Client::connect(address.as_ref()) {
                    Ok(client) => return Ok(client),
                    Err(e) => match e {
                        ttrpc::Error::Windows(231) => {
                            // ERROR_PIPE_BUSY
                            log::debug!("pipe busy during connect. try number {}", i);
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        _ => return Err(e.into()),
                    },
                }
            }
            Err(other!("failed to connect to {}", address.as_ref()))
        }
    }

    /// Publish a new event.
    ///
    /// Event object can be anything that Protobuf able serialize (e.g. implement `Message` trait).
    ///
    /// If containerd can't be reached or doesn't answer in time, the event is also queued to be
    /// retried in the background, and the error is returned.
    pub fn publish(
        &self,
        ctx: Context,
        topic: &str,
        namespace: &str,
        event: Box<dyn MessageDyn>,
    ) -> Result<()> {
        let mut envelope = Envelope::new();
        envelope.set_topic(topic.to_owned());
        envelope.set_namespace(namespace.to_owned());
        envelope.set_timestamp(timestamp()?);
        envelope.set_event(convert_to_any(event)?);

        let mut req = events::ForwardRequest::new();
        req.set_envelope(envelope);

        self.forward_or_requeue(ctx, req)?;

        Ok(())
    }

    fn forward_or_requeue(
        &self,
        ctx: Context,
        req: events::ForwardRequest,
    ) -> ttrpc::Result<empty::Empty> {
        let ctx = with_default_timeout(ctx);
        let err = match self.inner.forward(&ctx, &req) {
            Err(e) if is_retryable(&e) => e,
            res => return res,
        };

        let topic = req.envelope().topic().to_owned();
        let item = Item { ctx, req, count: 1 };
        match self.requeue.try_send(item) {
            Ok(()) => warn!("failed to publish event {topic}, will retry: {err}"),
            Err(TrySendError::Full(_)) => {
                error!("failed to publish event {topic}, dropping it as the retry queue is full: {err}")
            }
            Err(TrySendError::Disconnected(_)) => {
                error!("failed to publish event {topic}, dropping it as the retry thread exited: {err}")
            }
        }
        Err(err)
    }
}

impl Inner {
    /// Forward `req`. If the connection is gone, reconnect and resend once.
    fn forward(&self, ctx: &Context, req: &events::ForwardRequest) -> ttrpc::Result<empty::Empty> {
        // Clone the client so that the lock isn't held during the request.
        let (generation, client) = self.client.lock().unwrap().clone();
        match client.forward(ctx.clone(), req) {
            Err(e) if is_connection_error(&e) => {
                debug!("forward failed, reconnecting to {}: {}", self.address, e);
            }
            res => return res,
        }
        self.reconnect(generation)?.forward(ctx.clone(), req)
    }

    /// Replace the client of `generation`, which lost its connection. If another thread already
    /// replaced it, use that thread's client instead of connecting again.
    fn reconnect(&self, generation: u64) -> ttrpc::Result<EventsClient> {
        let mut current = self.client.lock().unwrap();
        if current.0 != generation {
            return Ok(current.1.clone());
        }
        let client = RemotePublisher::connect(&self.address)
            .map(EventsClient::new)
            // Report it as a connection error, so that the event is requeued.
            .map_err(|e| {
                ttrpc::Error::Socket(format!("failed to reconnect to {}: {}", self.address, e))
            })?;
        *current = (generation + 1, client.clone());
        Ok(client)
    }

    /// Retry requeued events until the publisher is dropped and no events are left. Events still
    /// waiting when the publisher is dropped keep their remaining retries.
    fn process_queue(&self, receiver: Receiver<Item>) {
        // Events waiting to be retried, with the time of their next try, in the order they failed.
        let mut pending: VecDeque<(Instant, Item)> = VecDeque::new();
        let mut open = true;
        loop {
            // Wait for a new event, or until the next try is due.
            let next = pending.iter().map(|(due, _)| *due).min();
            let received = match next {
                None if !open => break,
                None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
                Some(due) if open => {
                    receiver.recv_timeout(due.saturating_duration_since(Instant::now()))
                }
                // The publisher was dropped, so no new events can come.
                Some(due) => {
                    thread::sleep(due.saturating_duration_since(Instant::now()));
                    Err(RecvTimeoutError::Timeout)
                }
            };
            match received {
                Ok(item) if pending.len() >= QUEUE_SIZE => error!(
                    "dropping event {} as the retry queue is full",
                    item.req.envelope().topic()
                ),
                Ok(item) => pending.push_back((retry_time(&item), item)),
                Err(RecvTimeoutError::Disconnected) => open = false,
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.retry_due(&mut pending);
        }
        debug!("publisher retry thread exited");
    }

    /// Try the due events, in the order they failed.
    fn retry_due(&self, pending: &mut VecDeque<(Instant, Item)>) {
        let now = Instant::now();
        let mut waiting = VecDeque::with_capacity(pending.len());
        for (due, mut item) in pending.drain(..) {
            if due > now {
                waiting.push_back((due, item));
                continue;
            }
            let topic = item.req.envelope().topic().to_owned();
            match self.forward(&item.ctx, &item.req) {
                Ok(_) => debug!("published requeued event {topic}"),
                Err(e) if is_retryable(&e) && item.count < MAX_REQUEUE => {
                    debug!("failed to publish requeued event {topic}, will retry: {e}");
                    item.count += 1;
                    waiting.push_back((retry_time(&item), item));
                }
                Err(e) => error!("failed to publish event {topic}, dropping it: {e}"),
            }
        }
        *pending = waiting;
    }
}

impl Events for RemotePublisher {
    fn forward(
        &self,
        _ctx: &ttrpc::TtrpcContext,
        req: events::ForwardRequest,
    ) -> ttrpc::Result<empty::Empty> {
        self.forward_or_requeue(Context::default(), req)
    }
}

/// Whether `err` means the connection to containerd is gone. The sync ttrpc client reports
/// transport failures as `Socket`; the other variants are matched in case a ttrpc version returns
/// them.
fn is_connection_error(err: &ttrpc::Error) -> bool {
    matches!(
        err,
        ttrpc::Error::Socket(_)
            | ttrpc::Error::LocalClosed
            | ttrpc::Error::RemoteClosed
            | ttrpc::Error::Eof
    )
}

/// Whether a forward that failed with `err` is worth retrying: on a connection error, or on a
/// failure of the client itself (`Others`), which includes a timeout. An error status returned by
/// containerd (`RpcStatus`) is not.
///
/// Like the Go shim, this also retries client failures that can't succeed, such as a request over
/// the ttrpc message size limit, until the event is dropped: telling them apart from a timeout
/// would mean matching ttrpc's error messages.
fn is_retryable(err: &ttrpc::Error) -> bool {
    is_connection_error(err) || matches!(err, ttrpc::Error::Others(_))
}

fn with_default_timeout(mut ctx: Context) -> Context {
    if ctx.timeout_nano <= 0 {
        ctx.timeout_nano = FORWARD_TIMEOUT.as_nanos() as i64;
    }
    ctx
}

fn retry_time(item: &Item) -> Instant {
    Instant::now() + retry_delay(item.count)
}

/// Delay before the retry of an event that failed `count` times.
fn retry_delay(count: u32) -> Duration {
    RETRY_DELAY * count
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc, Barrier,
        },
        time::{Duration, Instant},
    };

    use client::{
        api::{Empty, ForwardRequest},
        events::task::TaskOOM,
        protobuf::Message,
    };
    use ttrpc::Server;

    use super::*;
    #[cfg(windows)]
    use crate::synchronous::wait_socket_working;

    /// Container id of an event that the server rejects with an error status.
    const REJECTED: &str = "rejected";
    /// Container id of an event that the server only answers after the forward timeout the first
    /// time, without recording it.
    const SLOW: &str = "slow";

    struct FakeServer {
        /// Receives the container id of each forwarded event, when set.
        tx: Option<mpsc::Sender<String>>,
        /// Whether an event with id `SLOW` was received.
        slow_seen: AtomicBool,
    }

    impl Events for FakeServer {
        fn forward(&self, _ctx: &ttrpc::TtrpcContext, req: ForwardRequest) -> ttrpc::Result<Empty> {
            let env = req.envelope();
            assert_eq!(env.topic(), "/tasks/oom");
            let Some(tx) = &self.tx else {
                return Ok(Empty::default());
            };
            let id = TaskOOM::parse_from_bytes(&env.event().value)
                .unwrap()
                .container_id()
                .to_owned();
            if id == SLOW && !self.slow_seen.swap(true, Ordering::SeqCst) {
                std::thread::sleep(FORWARD_TIMEOUT * 2);
                return Ok(Empty::default());
            }
            tx.send(id.clone()).unwrap();
            if id == REJECTED {
                return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::INVALID_ARGUMENT,
                    "rejected",
                )));
            }
            Ok(Empty::default())
        }
    }

    fn publish_oom(publisher: &RemotePublisher, container_id: &str) -> Result<()> {
        let mut msg = TaskOOM::new();
        msg.set_container_id(container_id.to_owned());
        publisher.publish(Context::default(), "/tasks/oom", "ns1", Box::new(msg))
    }

    /// Start a server on `path`, removing the socket a previous server left behind.
    fn start_server(path: &str, tx: &mpsc::Sender<String>) -> Server {
        #[cfg(unix)]
        let _ = std::fs::remove_file(path);
        let mut server = create_server(path, Some(tx.clone()));
        server.start().unwrap();
        #[cfg(windows)]
        // make sure pipe is ready on windows
        wait_socket_working(path, 5, 5).unwrap();
        server
    }

    /// Stop `server` like a containerd that exits, so that new connections fail.
    fn stop_server(server: Server, path: &str) {
        server.shutdown();
        #[cfg(unix)]
        std::fs::remove_file(path).unwrap();
        #[cfg(windows)]
        let _ = path;
    }

    #[cfg(unix)]
    fn test_address(tmpdir: &tempfile::TempDir) -> String {
        format!("{}/socket", tmpdir.path().to_str().unwrap())
    }

    /// Longest time a requeued event can take to be retried for the last time.
    fn retry_window() -> Duration {
        (1..=MAX_REQUEUE).map(retry_delay).sum()
    }

    /// Start a server and a publisher connected to it, with one event delivered, so that the
    /// server has set up the connection (else its shutdown can race with it).
    fn connected_publisher(
        path: &str,
        tx: &mpsc::Sender<String>,
        rx: &mpsc::Receiver<String>,
    ) -> (Server, RemotePublisher) {
        let server = start_server(path, tx);
        let publisher = RemotePublisher::new(path).unwrap();
        publish_oom(&publisher, "before").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "before");
        (server, publisher)
    }

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RemotePublisher>();
    }

    #[test]
    fn test_retry_delays() {
        // The test build's schedule: 1, 2, 3, 4 and 5 times the 100ms base delay, as in Go.
        let delays: Vec<_> = (1..=MAX_REQUEUE).map(retry_delay).collect();
        let expected: Vec<_> = [100, 200, 300, 400, 500].map(Duration::from_millis).into();
        assert_eq!(delays, expected);
    }

    #[test]
    fn test_reconnect() {
        #[cfg(unix)]
        let tmpdir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = test_address(&tmpdir);
        #[cfg(windows)]
        let path = "\\\\.\\pipe\\test-pipe-reconnect".to_string();
        let (tx, rx) = mpsc::channel();
        let (server, publisher) = connected_publisher(&path, &tx, &rx);

        // containerd restarts: the connection closes and a new server listens on the same address.
        server.shutdown();
        let server = start_server(&path, &tx);

        publish_oom(&publisher, "after").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "after");

        // The new connection is kept: publishing doesn't need to connect again.
        #[cfg(unix)]
        {
            std::fs::remove_file(&path).unwrap();
            publish_oom(&publisher, "kept").unwrap();
            assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "kept");
        }
        server.shutdown();
    }

    #[test]
    fn test_requeue() {
        #[cfg(unix)]
        let tmpdir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = test_address(&tmpdir);
        #[cfg(windows)]
        let path = "\\\\.\\pipe\\test-pipe-requeue".to_string();
        let (tx, rx) = mpsc::channel();
        let (server, publisher) = connected_publisher(&path, &tx, &rx);

        // containerd is down while the event is published: it is retried in the background.
        stop_server(server, &path);
        assert!(publish_oom(&publisher, "requeued").is_err());

        let server = start_server(&path, &tx);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            "requeued"
        );
        server.shutdown();
    }

    #[test]
    fn test_requeue_limit() {
        #[cfg(unix)]
        let tmpdir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = test_address(&tmpdir);
        #[cfg(windows)]
        let path = "\\\\.\\pipe\\test-pipe-requeue-limit".to_string();
        let (tx, rx) = mpsc::channel();
        let (server, publisher) = connected_publisher(&path, &tx, &rx);

        // containerd stays down for longer than the retries take: the event is dropped.
        stop_server(server, &path);
        assert!(publish_oom(&publisher, "dropped").is_err());
        // Restart before a 6th retry would be due.
        std::thread::sleep(retry_window() + RETRY_DELAY * 4);

        let server = start_server(&path, &tx);
        assert!(rx.recv_timeout(RETRY_DELAY * 10).is_err());
        server.shutdown();
    }

    #[test]
    fn test_requeue_burst() {
        #[cfg(unix)]
        let tmpdir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = test_address(&tmpdir);
        #[cfg(windows)]
        let path = "\\\\.\\pipe\\test-pipe-requeue-burst".to_string();
        let (tx, rx) = mpsc::channel();
        let (server, publisher) = connected_publisher(&path, &tx, &rx);

        // Requeuing more events must not make the first one retry before it is due, as that would
        // use up its retries at once.
        stop_server(server, &path);
        assert!(publish_oom(&publisher, "first").is_err());
        for i in 0..10 {
            assert!(publish_oom(&publisher, &format!("burst-{i}")).is_err());
        }
        std::thread::sleep(RETRY_DELAY * 2);

        let server = start_server(&path, &tx);
        while rx.recv_timeout(Duration::from_secs(10)).unwrap() != "first" {}
        server.shutdown();
    }

    #[test]
    fn test_requeue_after_drop() {
        #[cfg(unix)]
        let tmpdir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = test_address(&tmpdir);
        #[cfg(windows)]
        let path = "\\\\.\\pipe\\test-pipe-requeue-drop".to_string();
        let (tx, rx) = mpsc::channel();
        let (server, publisher) = connected_publisher(&path, &tx, &rx);

        // A requeued event is still retried after the publisher is dropped.
        stop_server(server, &path);
        assert!(publish_oom(&publisher, "requeued").is_err());
        drop(publisher);

        let server = start_server(&path, &tx);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            "requeued"
        );
        server.shutdown();
    }

    #[test]
    fn test_status_not_retried() {
        #[cfg(unix)]
        let tmpdir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = test_address(&tmpdir);
        #[cfg(windows)]
        let path = "\\\\.\\pipe\\test-pipe-status".to_string();
        let (tx, rx) = mpsc::channel();
        let (server, publisher) = connected_publisher(&path, &tx, &rx);

        match publish_oom(&publisher, REJECTED) {
            Err(Error::Ttrpc(ttrpc::Error::RpcStatus(_))) => {}
            res => panic!("expected an error status, got {res:?}"),
        }
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), REJECTED);
        // Neither resent nor requeued.
        assert!(rx.recv_timeout(retry_window() + RETRY_DELAY * 5).is_err());
        server.shutdown();
    }

    #[test]
    fn test_requeued_status_not_retried() {
        #[cfg(unix)]
        let tmpdir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = test_address(&tmpdir);
        #[cfg(windows)]
        let path = "\\\\.\\pipe\\test-pipe-requeued-status".to_string();
        let (tx, rx) = mpsc::channel();
        let (server, publisher) = connected_publisher(&path, &tx, &rx);

        // A requeued event that containerd rejects is dropped, not retried again.
        stop_server(server, &path);
        assert!(publish_oom(&publisher, REJECTED).is_err());
        let server = start_server(&path, &tx);
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), REJECTED);
        assert!(rx.recv_timeout(retry_window() + RETRY_DELAY * 5).is_err());
        server.shutdown();
    }

    #[test]
    fn test_caller_timeout_kept() {
        #[cfg(unix)]
        let tmpdir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = test_address(&tmpdir);
        #[cfg(windows)]
        let path = "\\\\.\\pipe\\test-pipe-caller-timeout".to_string();
        let (tx, rx) = mpsc::channel();
        let (server, publisher) = connected_publisher(&path, &tx, &rx);

        let timeout = FORWARD_TIMEOUT / 4;
        let mut msg = TaskOOM::new();
        msg.set_container_id(SLOW.to_owned());
        let ctx = ttrpc::context::with_timeout(timeout.as_nanos() as i64);
        let start = Instant::now();
        assert!(publisher
            .publish(ctx, "/tasks/oom", "ns1", Box::new(msg))
            .is_err());
        let elapsed = start.elapsed();
        assert!(
            elapsed >= timeout && elapsed < FORWARD_TIMEOUT,
            "publish took {elapsed:?}"
        );
        server.shutdown();
    }

    #[test]
    fn test_retry_thread_exits() {
        #[cfg(unix)]
        let tmpdir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = test_address(&tmpdir);
        #[cfg(windows)]
        let path = "\\\\.\\pipe\\test-pipe-thread-exits".to_string();
        let (tx, rx) = mpsc::channel();
        let (server, publisher) = connected_publisher(&path, &tx, &rx);

        // The retry thread holds `inner` until it exits.
        let inner = Arc::downgrade(&publisher.inner);
        drop(publisher);
        let start = Instant::now();
        while inner.upgrade().is_some() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "retry thread still running"
            );
            std::thread::sleep(RETRY_DELAY);
        }
        server.shutdown();
    }

    #[test]
    fn test_timeout_requeued() {
        #[cfg(unix)]
        let tmpdir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = test_address(&tmpdir);
        #[cfg(windows)]
        let path = "\\\\.\\pipe\\test-pipe-timeout".to_string();
        let (tx, rx) = mpsc::channel();
        let (server, publisher) = connected_publisher(&path, &tx, &rx);

        // The forward times out after the default timeout, without being resent at once, and the
        // event is then retried in the background.
        let start = Instant::now();
        assert!(publish_oom(&publisher, SLOW).is_err());
        let elapsed = start.elapsed();
        assert!(
            elapsed >= FORWARD_TIMEOUT && elapsed < FORWARD_TIMEOUT * 2,
            "publish took {elapsed:?}"
        );
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), SLOW);
        server.shutdown();
    }

    #[test]
    fn test_connect() {
        #[cfg(unix)]
        let tmpdir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let path = format!("{}/socket", tmpdir.as_ref().to_str().unwrap());
        #[cfg(windows)]
        let path = "\\\\.\\pipe\\test-pipe".to_string();
        let path1 = path.clone();

        assert!(RemotePublisher::connect("a".repeat(16384)).is_err());
        assert!(RemotePublisher::connect(&path).is_err());

        let barrier = Arc::new(Barrier::new(2));
        let barrier2 = barrier.clone();
        let thread = std::thread::spawn(move || {
            let mut server = create_server(&path1, None);

            server.start().unwrap();

            #[cfg(windows)]
            // make sure pipe is ready on windows
            wait_socket_working(&path1, 5, 5).unwrap();

            barrier2.wait();

            barrier2.wait();
            server.shutdown();
        });

        barrier.wait();
        let client = RemotePublisher::new(&path).unwrap();
        let mut msg = TaskOOM::new();
        msg.set_container_id("test".to_string());
        client
            .publish(Context::default(), "/tasks/oom", "ns1", Box::new(msg))
            .unwrap();
        barrier.wait();

        thread.join().unwrap();
    }

    fn create_server(server_address: &str, tx: Option<mpsc::Sender<String>>) -> Server {
        #[cfg(unix)]
        {
            use std::os::unix::{io::AsRawFd, net::UnixListener};
            let listener = UnixListener::bind(server_address).unwrap();
            listener.set_nonblocking(true).unwrap();
            let task = Box::new(FakeServer {
                tx,
                slow_seen: AtomicBool::new(false),
            }) as Box<dyn Events + Send + Sync>;
            let service = client::create_events(task.into());
            let server = Server::new()
                .add_listener(listener.as_raw_fd())
                .unwrap()
                .register_service(service);
            std::mem::forget(listener);
            server
        }

        #[cfg(windows)]
        {
            let service = client::create_events(Arc::new(FakeServer {
                tx,
                slow_seen: AtomicBool::new(false),
            }));

            Server::new()
                .bind(server_address)
                .unwrap()
                .register_service(service)
        }
    }
}
