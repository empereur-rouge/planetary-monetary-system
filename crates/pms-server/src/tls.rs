use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer},
};
use rustls_pemfile::{certs, ec_private_keys, pkcs8_private_keys};
use std::{fs::File, io::BufReader};

pub fn load_tls(cert_path: &str, key_path: &str) -> anyhow::Result<ServerConfig> {
    // certs
    let mut cr = BufReader::new(File::open(cert_path)?);
    let cert_chain: Vec<CertificateDer<'static>> = certs(&mut cr).collect::<Result<_, _>>()?;

    // keys: PKCS#8 d’abord
    let mut kr = BufReader::new(File::open(key_path)?);
    let mut keys: Vec<PrivateKeyDer<'static>> = pkcs8_private_keys(&mut kr)
        .map(|r| r.map(PrivateKeyDer::from))
        .collect::<Result<_, _>>()?;

    // fallback SEC1 (EC PRIVATE KEY)
    if keys.is_empty() {
        kr = BufReader::new(File::open(key_path)?);
        keys = ec_private_keys(&mut kr)
            .map(|r| r.map(PrivateKeyDer::from))
            .collect::<Result<_, _>>()?;
    }

    anyhow::ensure!(!keys.is_empty(), "no private key found (pkcs8/ec)");
    let key = keys.remove(0);

    let mut cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)?;
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}

pub fn load_client_config(
    cert_path: &str,
    key_path: &str,
    ca_path: Option<&str>,
) -> anyhow::Result<rustls::ClientConfig> {
    // 1. Load RootCertStore (CA)
    let mut root_store = rustls::RootCertStore::empty();

    // Add WebPKI roots (optional, but good for real TLS)
    // root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    // Add Custom CA if present
    if let Some(ca) = ca_path {
        let mut cr = BufReader::new(File::open(ca)?);
        for cert in certs(&mut cr) {
            root_store.add(cert?)?;
        }
    }

    // 2. Load Client Cert/Key (Mutual TLS)
    let mut cr = BufReader::new(File::open(cert_path)?);
    let cert_chain: Vec<CertificateDer<'static>> = certs(&mut cr).collect::<Result<_, _>>()?;

    let mut kr = BufReader::new(File::open(key_path)?);
    let mut keys: Vec<PrivateKeyDer<'static>> = pkcs8_private_keys(&mut kr)
        .map(|r| r.map(PrivateKeyDer::from))
        .collect::<Result<_, _>>()?;

    if keys.is_empty() {
        kr = BufReader::new(File::open(key_path)?);
        keys = ec_private_keys(&mut kr)
            .map(|r| r.map(PrivateKeyDer::from))
            .collect::<Result<_, _>>()?;
    }
    anyhow::ensure!(!keys.is_empty(), "no private key found (pkcs8/ec)");
    let key = keys.remove(0);

    // 3. Build ClientConfig
    let mut cfg = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_client_auth_cert(cert_chain, key)?;

    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}
