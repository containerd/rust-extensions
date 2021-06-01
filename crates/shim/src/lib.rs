use containerd_protos::shim::shim_ttrpc::Task;

#[derive(Debug, Default)]
pub struct StartOpts {
    pub id: String,
    pub containerd_binary: String,
    pub address: String,
    pub ttrpc_address: String,
}

pub trait Shim: Task {
    fn new(id: String, namespace: String) -> Self;

    fn start_shim(opts: StartOpts);
    fn cleanup();
}

pub fn run<T>(id: String)
where
    T: Shim,
{
    let _shim = T::new(id, "".to_string());
}
