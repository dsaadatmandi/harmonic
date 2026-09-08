use std::{fs, io, net::SocketAddr, path::PathBuf};

use tonic::transport::{Certificate, Channel};
use walkdir::WalkDir;

use crate::{
    proto::{CertificateRequest, bootstrap_client::BootstrapClient},
    sync::config::{config_dir_path, ensure_config_dir},
    utils::{HarmonicError, Result},
};

pub fn load_cert() -> Result<Certificate> {
    let path = get_cert_path();
    for entry in WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|f| {
            f.file_name()
                .to_str()
                .map(|s| s == "server.crt")
                .unwrap_or(false)
        })
    {
        if let Ok(pem) = fs::read(entry.path()) {
            return Ok(Certificate::from_pem(pem));
        }
    }

    Err(HarmonicError::NotFound(
        "server.crt certificate file".to_string(),
    ))
}

pub async fn bootstrap_from_server(server_address: &String) -> Result<Certificate> {
    let otp = get_user_input_for_otp()?;

    let channel = Channel::from_shared(format_bootstrap_address(
        server_address,
        get_user_input_for_bootstrap_server_port(),
    )?)?
    .connect()
    .await?;

    let mut client = BootstrapClient::new(channel);

    let response = client
        .get_certificate(CertificateRequest { otp: otp })
        .await?;

    let pem_bytes = response.into_inner().certificate_pem;

    save_cert(&pem_bytes)
}

pub fn save_cert(pem_bytes: &[u8]) -> Result<Certificate> {
    ensure_config_dir()?;
    fs::write(get_cert_path(), pem_bytes)?;

    Ok(Certificate::from_pem(pem_bytes))
}

fn get_cert_path() -> PathBuf {
    let mut base_path = config_dir_path().unwrap_or(PathBuf::from("./.harmonic"));
    base_path.push("server.crt");

    base_path
}

fn get_user_input_for_bootstrap_server_port() -> u16 {
    let mut buf = String::new();
    let port: u16 = loop {
        buf.clear();
        println!("Please enter bootstrap server port (or just press enter for default value): ");

        if io::stdin().read_line(&mut buf).is_err() {
            println!(
                "Error reading from stdin. Try again but this may be an unrecoverable failure."
            );
            continue;
        };

        match buf.trim() {
            "" => break 42070,
            s => match s.parse() {
                Ok(p) => break p,
                Err(_) => {
                    println!("Could not parse port. Try again.");
                    continue;
                }
            },
        }
    };
    port
}

fn format_bootstrap_address(server_address: &String, port: u16) -> Result<String> {
    let mut addr: SocketAddr = server_address
        .parse()
        .map_err(|_| HarmonicError::InvalidInputError)?;

    addr.set_port(port);

    Ok(format!("http://{}", addr))
}

fn get_user_input_for_otp() -> Result<String> {
    read_otp(&mut io::stdin().lock())
}

fn read_otp<R: io::BufRead>(input: &mut R) -> Result<String> {
    println!("First time setup");

    loop {
        println!("Enter OTP from server to download certificate for TLS connection: ");

        let mut buf = String::new();
        let read = input.read_line(&mut buf).map_err(HarmonicError::Io)?;

        if read == 0 {
            // stdin closed without an otp, e.g. a headless launcher
            return Err(HarmonicError::Input(String::from(
                "input closed before an otp was provided",
            )));
        }

        let trimmed = buf.trim();
        if trimmed.len() == 64 {
            return Ok(trimmed.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_read_otp_accepts_64_characters() {
        let otp = "a".repeat(64);
        let mut input = Cursor::new(format!("{otp}\n"));

        assert_eq!(read_otp(&mut input).unwrap(), otp);
    }

    #[test]
    fn test_read_otp_retries_on_invalid_length() {
        let otp = "b".repeat(64);
        let mut input = Cursor::new(format!("tooshort\n{otp}\n"));

        assert_eq!(read_otp(&mut input).unwrap(), otp);
    }

    #[test]
    fn test_read_otp_fails_on_closed_input() {
        let mut input = Cursor::new("");

        let result = read_otp(&mut input);

        assert!(result.is_err(), "closed input must fail instead of looping forever");
    }
}
