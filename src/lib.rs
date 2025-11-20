pub mod client;
pub mod server;
pub mod sync;
pub mod utils;

pub mod proto {
    tonic::include_proto!("harmonic");
}

// Re-export commonly used items
pub use sync::{Config, load_config};
