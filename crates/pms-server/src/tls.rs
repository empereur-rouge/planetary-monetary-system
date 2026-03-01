use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use std::fs;

/// Load TLS server configuration from PEM files.
/// Uses rustls native PEM support (replaces deprecated rustls-pemfile crate).
pub fn load_tls(cert_path: &str, key_path: &str) -> anyhow::Result<ServerConfig> {
    // Load certificates from PEM file
    let cert_pem = fs::read(cert_path)?;
    let cert_chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(&cert_pem).collect::<Result<_, _>>()?;

    // Load private key (supports PKCS#8, SEC1/EC, and RSA formats automatically)
    let key_pem = fs::read(key_path)?;
    let key = PrivateKeyDer::from_pem_slice(&key_pem)?;

    let mut cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)?;
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}

/// Load TLS client configuration for mutual TLS.
/// Uses rustls native PEM support (replaces deprecated rustls-pemfile crate).
pub fn load_client_config(
    cert_path: &str,
    key_path: &str,
    ca_path: Option<&str>,
) -> anyhow::Result<rustls::ClientConfig> {
    // 1. Load RootCertStore (CA)
    let mut root_store = rustls::RootCertStore::empty();

    // Add Custom CA if present
    if let Some(ca) = ca_path {
        let ca_pem = fs::read(ca)?;
        for cert in CertificateDer::pem_slice_iter(&ca_pem) {
            root_store.add(cert?)?;
        }
    }

    // 2. Load Client Cert/Key (Mutual TLS)
    let cert_pem = fs::read(cert_path)?;
    let cert_chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(&cert_pem).collect::<Result<_, _>>()?;

    let key_pem = fs::read(key_path)?;
    let key = PrivateKeyDer::from_pem_slice(&key_pem)?;

    // 3. Build ClientConfig
    let mut cfg = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_client_auth_cert(cert_chain, key)?;

    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}
