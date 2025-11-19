pub mod common;
pub mod watcher;
pub mod error;

pub mod harmonic {
    tonic::include_proto!("harmonic");
}