use std::path::PathBuf;

use harmonic::sync::*;

pub fn create_test_config(path: &PathBuf) -> Config {
    Config {
        sync_path: path.clone(),
        socket_addr: String::from("[::1]:42069"),
        schedule_delay: 10,
        log_level: String::from("debug"),
        sync_threshold: 20,
        modify_weight: 2,
        remove_weight: 5,
        create_weight: 10,
        block_size: 8192,
    }
}
