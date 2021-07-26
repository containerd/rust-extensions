use std::env;

use containerd_shim_protos as client;

fn main() {
    let args: Vec<String> = env::args().collect();

    let socket_path = args
        .get(1)
        .ok_or("First argument must be shim socket path")
        .unwrap();

    let pid = args
        .get(2)
        .and_then(|str| Some(str.to_owned()))
        .unwrap_or_default();

    let client = client::Client::connect(socket_path).expect("Failed to connect to shim");

    let task_client = client::TaskClient::new(client);

    let context = client::ttrpc::context::with_timeout(0);

    let req = client::api::ConnectRequest {
        id: pid,
        ..Default::default()
    };

    let resp = task_client
        .connect(context, &req)
        .expect("Connect request failed");

    println!("Resp: {:?}", resp);
}
