use anyhow::{Context, Result};

use clap::Parser;

use harmonic::server::config::create_server_config;
use harmonic::server::get_server_tls_config;
use harmonic::server::service::HarmonicService;
use harmonic::sync::{self};
use harmonic::utils::tracing::*;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::transport::Server;
use tower::ServiceBuilder;
use tower_http::trace::TraceLayer;
use tracing::{debug, error, info};

#[cfg(feature = "compression-zstd")]
use tonic::codec::CompressionEncoding;

use harmonic::proto::harmonic_server::HarmonicServer;
use harmonic::sync::config::config_dir_path;

#[derive(Parser, Debug)]
#[command(name = "harmonic-server")]
#[command(about = "Harmonic file synchronization server", long_about = None)]
struct Args {
    #[arg(long)]
    bootstrap: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = Args::parse();

    let config = sync::load_config().context("Failed to load config")?;

    tracing_orchestrator(&config.log_level);

    info!("Starting server");

    debug!("Address from config: {:?}", config.socket_addr);

    let (tls_config, generated_cert) = get_server_tls_config(&config)?;

    // overwrite above values with new.
    debug!("Injecting local address into config for serving");
    let config = create_server_config().context("Unable to inject server address into config.")?;
    let address = config.socket_addr()?;

    let cert_path = config_dir_path()
        .unwrap_or(config.sync_path.join(".harmonic"))
        .join("certificate.crt");

    let bootstrap_address = {
        let mut addr = address;
        addr.set_port(42070);
        addr
    };

    let harmonic = HarmonicService {
        sync_sessions: Arc::new(Mutex::new(Default::default())),
        config,
    };

    let service = HarmonicServer::new(harmonic);

    #[cfg(feature = "compression-zstd")]
    let service = service
        .send_compressed(CompressionEncoding::Zstd)
        .accept_compressed(CompressionEncoding::Zstd);

    debug!("Building server");

    let main_server = Server::builder()
        .tls_config(tls_config)
        .context("Error in adding tls layer")?
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_grpc().make_span_with(make_span))
                .map_request(accept_trace),
        )
        .add_service(service)
        .serve(address);

    info!("Main server started on {}", address);

    // run bootstrap if --bootstrap arg or certificate was generated this run
    let should_run_bootstrap = args.bootstrap || generated_cert;

    if should_run_bootstrap {
        info!("Starting bootstrap server on {}", bootstrap_address);
        tokio::spawn(async move {
            match harmonic::server::run_bootstrap_server(cert_path, bootstrap_address).await {
                Ok(()) => info!("Bootstrap server stopped"),
                Err(e) => error!("Bootstrap server failed: {:?}", e),
            }
        });
    } else {
        info!("Bootstrap server disabled. Use --bootstrap flag to enable.");
    }

    main_server.await?;
    info!("Main server stopped");

    Ok(())
}
