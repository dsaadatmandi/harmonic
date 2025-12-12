use std::{fs, path::PathBuf, sync::Arc};

use rustls::client::danger::{ServerCertVerified, ServerCertVerifier};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::utils::Result;

#[derive(Debug)]
struct FingerprintStore {
    path: PathBuf,
    fingerprints: Vec<Fingerprint>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Fingerprint {
    hash: [u8; 32],
}

#[derive(Debug)]
struct SelfSignedCertVerifier {
    fingerprint_store: Arc<FingerprintStore>,
}


impl FingerprintStore {
    pub fn load(path: &PathBuf) -> Self {
        let mut store: Vec<Fingerprint> = Vec::new();
        for entry in WalkDir::new(path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|f| {
                f.file_name()
                    .to_str()
                    .map(|s| s.ends_with(".prnt"))
                    .unwrap_or(false)
            })
        {
            match fs::File::open(entry.into_path()) {
                Ok(f) => match serde_json::from_reader(f) {
                    Ok(fingerprint) => store.push(fingerprint),
                    Err(_) => continue,
                },
                Err(_) => continue,
            }
        }
        Self {
            path: path.to_path_buf(),
            fingerprints: store,
        }
    }

    pub fn check(&self, hash: [u8; 32]) -> bool {
        self.fingerprints.iter().any(|f| f.hash == hash)
    }

    pub fn store(&self, hash: [u8; 32]) -> Result<bool> {
        if !Self::trigger_user_verification() {
            return Ok(false)
        }
        let fp = Fingerprint { hash };
        let mut pout = self.path.join(uuid::Uuid::new_v4().to_string());
        pout.set_extension(".prnt");
        let fout = fs::File::create(pout)?;

        serde_json::to_writer(fout, &fp)?;
        Ok(true)
    }

    pub fn trigger_user_verification() -> bool {
        // WORK IN PROGRESS
        println!("Unknown fingerprint encountered");
        println!("Information:");

        println!("Should the certificate be saved? (y/n)");

        true
    }
}

impl ServerCertVerifier for SelfSignedCertVerifier {
    fn verify_server_cert(
            &self,
            end_entity: &tonic::transport::CertificateDer<'_>,
            _intermediates: &[tonic::transport::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp_response: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        
        let cert_bt = end_entity.as_ref();

        let hash: [u8; 32] = blake3::hash(cert_bt).into();

        if self.fingerprint_store.check(hash) {
            return Ok(ServerCertVerified::assertion());
        }

        if let Ok(stored) = self.fingerprint_store.store(hash) {
            if stored {
                return Ok(ServerCertVerified::assertion());
            }
        }

        Err(rustls::Error::General("Certificate not known and not accepted by user".to_string()))
    }
    
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &tonic::transport::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        todo!()
    }
    
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &tonic::transport::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        todo!()
    }
    
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        todo!()
    }

}
