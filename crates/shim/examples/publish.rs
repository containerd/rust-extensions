use std::env;

use containerd_shim::{ttrpc::context::Context, RemotePublisher};
use containerd_shim_protos::events::task::TaskOOM;

fn main() {
    let args: Vec<String> = env::args().collect();

    // Must not start with unix://
    let address = args
        .get(1)
        .ok_or("First argument must be containerd's TTRPC address to publish events")
        .unwrap();

    println!("Connecting: {}", &address);

    let publisher = RemotePublisher::new(address).expect("Connect failed");

    let mut event = TaskOOM::new();
    event.set_container_id("123".into());

    let ctx = Context::default();

    println!("Sending event");
    publisher
        .publish(ctx, "/tasks/oom", "default", event)
        .expect("Publish failed");

    println!("Done");
}
