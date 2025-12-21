use std::{fs, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use futures::lock::Mutex;
use rand::{Rng, distr::Alphanumeric};
use tokio::time::Instant;
use tonic::{Request, Response, Status, transport::Server};

use crate::proto::{
    CertificateRequest, CertificateResponse,
    bootstrap_server::{Bootstrap, BootstrapServer},
};
use crate::utils::Result;

pub struct BootstrapService {
    otp: Arc<Mutex<Option<(String, Instant)>>>,
    cert_path: PathBuf,
}

impl BootstrapService {
    pub fn new(cert_path: PathBuf, otp: String) -> Self {
        Self {
            otp: Arc::new(Mutex::new(Some((otp, Instant::now())))),
            cert_path,
        }
    }
}

#[tonic::async_trait]
impl Bootstrap for BootstrapService {
    async fn get_certificate(
        &self,
        request: Request<CertificateRequest>,
    ) -> std::result::Result<Response<CertificateResponse>, Status> {
        let received_otp = request.into_inner().otp;

        let mut saved_otp = self.otp.lock().await;

        if let Some((valid_otp, creation_instant)) = saved_otp.as_ref() {
            if Instant::now().duration_since(*creation_instant) > Duration::from_secs(300) {
                *saved_otp = None;
                return Err(Status::deadline_exceeded(
                    "OTP Password for Bootstrapping certificate has expired",
                ));
            }

            if *valid_otp == received_otp {
                // consuming, could implement another invalidation patterns
                *saved_otp = None;

                let cert_bytes = fs::read(&self.cert_path).map_err(|e| {
                    Status::internal(format!("Could not read cert from disk: {}", e))
                })?;

                return Ok(Response::new(CertificateResponse {
                    certificate_pem: cert_bytes,
                }));
            }
        }
        Err(Status::permission_denied("OTP did not match"))
    }
}

pub fn generate_otp(length: usize) -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
        .collect()
}

pub fn display_otp(otp: &str, addr: &SocketAddr) {
    println!("================================");
    println!("Bootstrap server running on: {}", addr);
    println!(
        "OTP password to copy self-signed certificate from server: {}",
        otp
    );
    println!(
        "Enter this on the client after startup. To overwrite existing cert, run client with --bootstrap flag"
    );
    println!("================================");
}

pub async fn run_bootstrap_server(cert_path: PathBuf, bootstrap_addr: SocketAddr) -> Result<()> {
    let otp = generate_otp(64);
    display_otp(&otp, &bootstrap_addr);

    let bootstrap_service = BootstrapServer::new(BootstrapService::new(cert_path, otp));

    Server::builder()
        .add_service(bootstrap_service)
        .serve(bootstrap_addr)
        .await?;

    Ok(())
}
