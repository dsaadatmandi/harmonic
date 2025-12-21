use std::{fs, net::SocketAddr};

use rcgen::{CertifiedKey, generate_simple_self_signed};
use tonic::transport::{Identity, ServerTlsConfig};

use crate::{
    Config,
    sync::config::config_dir_path,
    utils::{HarmonicError, Result},
};

pub fn get_identity(config: &Config) -> Result<Identity> {
    let config_dir = config_dir_path().unwrap_or(config.sync_path.join(".harmonic"));
    let cert_path = config_dir.join("certificate.crt");
    let private_key_path = config_dir.join("certificate.pk");

    if cert_path.exists() && private_key_path.exists() {
        let cert = fs::read(cert_path)?;
        let key = fs::read(private_key_path)?;

        Ok(Identity::from_pem(cert, key))
    } else {
        let simple_cert_pre = vec![
            // adding whatever address is provided by the configuration and local ip address
            config.socket_addr()?.to_string(),
            SocketAddr::new(
            local_ip_address::local_ip().map_err(|e| HarmonicError::CryptoError(e.to_string()))?,
            config.socket_addr()?.port(),
        )
        .to_string()];

        let CertifiedKey { cert, signing_key } = generate_simple_self_signed(simple_cert_pre)
            .map_err(|e| HarmonicError::CryptoError(e.to_string()))?;
        fs::write(cert_path, cert.pem())?;
        fs::write(private_key_path, signing_key.serialize_pem())?;

        Ok(Identity::from_pem(cert.pem(), signing_key.serialize_pem()))
    }
}

pub fn get_server_tls_config(config: &Config) -> Result<ServerTlsConfig> {
    let identity = get_identity(&config)?;
    Ok(ServerTlsConfig::new().identity(identity))
}
