// End-to-end client server sync test
//
// Runs a real TLS harmonic server in process and drives the client sync
// execution against it: state exchange, sync plan, file upload, file
// download and state persistence. Runs in a single test function because
// state paths are relative to the process working directory

use harmonic::client::sync::run_sync;
use harmonic::proto::harmonic_server::HarmonicServer;
use harmonic::server::{get_server_tls_config, HarmonicService};
use harmonic::sync::{load_state, Config};
use rcgen::{generate_simple_self_signed, CertifiedKey};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_client_sync_exchanges_files_with_server() {
    let process_dir = tempdir().unwrap();
    std::env::set_current_dir(process_dir.path()).unwrap();

    let server_dir = tempdir().unwrap();
    let client_dir = tempdir().unwrap();
    let server_root = PathBuf::from(server_dir.path());
    let client_root = PathBuf::from(client_dir.path());

    // client and server each hold a file the other is missing
    std::fs::write(client_root.join("note.txt"), b"from client").unwrap();
    std::fs::write(server_root.join("book.txt"), b"from server").unwrap();

    // pre-generate the server identity so both sides share one certificate
    install_identity("127.0.0.1");

    // rustls needs a crypto provider outside the binary entrypoint
    let _ = rustls::crypto::ring::default_provider().install_default();

    let server_config = Config {
        sync_path: server_root.clone(),
        socket_addr: String::from("127.0.0.1:0"),
        schedule_delay: 100,
        log_level: String::from("debug"),
        sync_threshold: 20,
        modify_weight: 2,
        remove_weight: 5,
        create_weight: 10,
        block_size: 8192,
    };
    let (tls_config, _) = get_server_tls_config(&server_config).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let service = HarmonicServer::new(HarmonicService {
        sync_sessions: Arc::new(Mutex::new(HashMap::new())),
        config: server_config,
    });

    #[cfg(feature = "compression-zstd")]
    let service = service
        .send_compressed(tonic::codec::CompressionEncoding::Zstd)
        .accept_compressed(tonic::codec::CompressionEncoding::Zstd);

    let server = Server::builder()
        .tls_config(tls_config)
        .unwrap()
        .add_service(service)
        .serve_with_incoming(TcpListenerStream::new(listener));

    tokio::spawn(server);

    let client_config = Config {
        sync_path: client_root.clone(),
        socket_addr: format!("127.0.0.1:{}", addr.port()),
        schedule_delay: 100,
        log_level: String::from("debug"),
        sync_threshold: 20,
        modify_weight: 2,
        remove_weight: 5,
        create_weight: 10,
        block_size: 8192,
    };

    run_sync(&client_config, false)
        .await
        .unwrap_or_else(|e| panic!("run_sync failed: {e:#}"));

    // client file was uploaded
    assert_eq!(
        std::fs::read(server_root.join("note.txt")).unwrap(),
        b"from client"
    );

    // server file was downloaded
    assert_eq!(
        std::fs::read(client_root.join("book.txt")).unwrap(),
        b"from server"
    );

    // the persisted state reflects the pre transfer client tree, the
    // downloaded file joins it on the next pass where the hashes converge
    let state = load_state().unwrap();
    assert!(state.tree.contains_key(&PathBuf::from("note.txt")));
}

/// Generates the server certificate and key and trusts it on the client side,
/// mirroring the bootstrap flow without the interactive otp prompt
fn install_identity(san: &str) {
    let config_dir = Path::new(".harmonic");
    std::fs::create_dir_all(config_dir).unwrap();

    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec![san.to_string()]).unwrap();

    std::fs::write(config_dir.join("certificate.crt"), cert.pem()).unwrap();
    std::fs::write(config_dir.join("certificate.pk"), signing_key.serialize_pem()).unwrap();
    std::fs::write(config_dir.join("server.crt"), cert.pem()).unwrap();
}
