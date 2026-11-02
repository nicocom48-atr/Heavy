pub mod automatic;
pub mod connectivity;
pub mod discovery;
pub mod integrity;
pub mod manager;
pub mod network;
pub mod pairing;
pub mod part_file;
pub mod protocol;
pub mod receiver;
pub mod sender;
pub mod server;
pub mod session;
pub mod signaling;
pub mod speed_limiter;
pub mod transfer_state;
mod task_guard;

/// Initialize tracing subscriber compatible with RUST_LOG
pub fn init_logging(verbose: bool) {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
    let default_filter = if verbose {
        "heavydrop=debug,iroh=info,info"
    } else {
        "heavydrop=info,warn"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .try_init();
}
