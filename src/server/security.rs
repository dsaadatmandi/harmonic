use std::{fs};

use rcgen::{CertifiedKey, generate_simple_self_signed};
use tonic::transport::{Identity, ServerTlsConfig};

use crate::{
    Config,
    sync::config::ensure_config_dir,
    utils::{HarmonicError, Result},
};

pub fn get_identity(config: &Config) -> Result<(Identity, bool)> {
    let config_dir = ensure_config_dir()?;
    let cert_path = config_dir.join("certificate.crt");
    let private_key_path = config_dir.join("certificate.pk");

    if cert_path.exists() && private_key_path.exists() {
        let cert = fs::read(cert_path)?;
        let key = fs::read(private_key_path)?;

        Ok((Identity::from_pem(cert, key), false))
    } else {
        let simple_cert_pre = vec![
            // adding whatever address is provided by the configuration and local ip address
            config.socket_addr()?.ip().to_string(),
            local_ip_address::local_ip()
                .map_err(|e| HarmonicError::CryptoError(e.to_string()))?
                .to_string(),
        ];

        let CertifiedKey { cert, signing_key } = generate_simple_self_signed(simple_cert_pre)
            .map_err(|e| HarmonicError::CryptoError(e.to_string()))?;
        fs::write(cert_path, cert.pem())?;
        fs::write(private_key_path, signing_key.serialize_pem())?;

        Ok((Identity::from_pem(cert.pem(), signing_key.serialize_pem()), true))
    }
}

pub fn get_server_tls_config(config: &Config) -> Result<(ServerTlsConfig, bool)> {
    let (identity, was_generated) = get_identity(config)?;
    Ok((ServerTlsConfig::new().identity(identity), was_generated))
}
