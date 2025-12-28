use std::format;
use k256::ecdsa::SigningKey;

#[derive(Debug)]
pub enum SignError {
    Base64Decode,
    InvalidLength,
    SigningKey,
}

#[derive(Debug)]
pub enum VerifyError {
    Base64Decode,
    SignatureFormat,
    HexDecode,
    InvalidPubKey,
}

pub trait SignerBackend {
    fn sign(&self, message: &str) -> Result<String, SignError>;
    fn verify(&self, message: &str, signature: &str) -> Result<bool, VerifyError>;
}