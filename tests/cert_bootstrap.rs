// TLS certificate persistence tests
//
// Certificates are written into the .harmonic config directory. The directory
// must be created on demand, matching how config and state files behave, so a
// fresh install or a process started from a different working directory does
// not fail while bootstrapping. Runs in a single test function because the
// config directory is resolved relative to the process working directory

use harmonic::client::save_cert;
use harmonic::server::get_server_tls_config;
use harmonic::sync::Config;
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn test_certificates_are_written_without_existing_config_dir() {
    let dir = tempdir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    let saved = save_cert(b"test-certificate-pem");
    assert!(saved.is_ok(), "save_cert must create the config directory: {saved:?}");
    assert!(dir.path().join(".harmonic/server.crt").exists());

    let config = Config {
        sync_path: PathBuf::from(dir.path()),
        socket_addr: String::from("127.0.0.1:42069"),
        schedule_delay: 100,
        log_level: String::from("info"),
        sync_threshold: 20,
        modify_weight: 2,
        remove_weight: 5,
        create_weight: 10,
        block_size: 8192,
    };

    let tls = get_server_tls_config(&config);
    assert!(tls.is_ok(), "server identity generation must create the config directory: {tls:?}");
    assert!(dir.path().join(".harmonic/certificate.crt").exists());
    assert!(dir.path().join(".harmonic/certificate.pk").exists());
}
