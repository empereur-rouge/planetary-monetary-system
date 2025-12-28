use std::{fs::File, io::BufReader};
use std::path::Path;
use rustls::{pki_types::{CertificateDer, PrivateKeyDer}, ServerConfig};
use rustls_pemfile::{certs, pkcs8_private_keys, ec_private_keys};


pub fn load_tls(cert_path: &str, key_path: &str) -> anyhow::Result<ServerConfig> {
    // certs
    let mut cr = BufReader::new(File::open(cert_path)?);
    let cert_chain: Vec<CertificateDer<'static>> =
        certs(&mut cr).collect::<Result<_, _>>()?;

    // keys: PKCS#8 d’abord
    let mut kr = BufReader::new(File::open(key_path)?);
    let mut keys: Vec<PrivateKeyDer<'static>> =
        pkcs8_private_keys(&mut kr)
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