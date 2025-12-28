use pms_errors::ValidationError;
use pms_types::Transaction;

pub fn verify_tx_signatures(_tx: &Transaction) -> Result<(), ValidationError> {
    // TODO: appeler pms-crypto (ed25519/secp256k1) + format signature clair.
    Ok(())
}