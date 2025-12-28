
//! # PMS Wallet - Crate principal
//!
//! Ce crate permet la création, la sauvegarde et l'utilisation d'un wallet basé
//! sur une phrase mnémotechnique (BIP-39). Il fournit également une interface
//! abstraite [`SignerBackend`] pour changer facilement d'algorithme de cryptographie.

/// Erreurs possibles lors de la signature d’un message.
#[derive(Debug)]
pub enum SignError {
    /// La clé privée est invalide en base64.
    Base64Decode,
    /// Mauvaise longueur de clé privée.
    InvalidLength,
    /// La génération de la clé de signature a échoué.
    SigningKey,
}

/// Erreurs possibles lors de la vérification d’une signature.
#[derive(Debug)]
pub enum VerifyError {
    /// La signature fournie n’est pas encodée en base64.
    Base64Decode,
    /// Le format DER de la signature est invalide.
    SignatureFormat,
    /// La clé publique n’est pas décodable en hexadécimal.
    HexDecode,
    /// La clé publique est mal formée ou invalide.
    InvalidPubKey,
}

/// Interface abstraite pour les algorithmes de signature.
///
/// Permet de découpler la logique du wallet et l'algorithme de cryptographie utilisé (k256, ed25519, etc).
pub trait SignerBackend {
    /// Signe un message et retourne la signature encodée.
    ///
    /// # Arguments
    /// * `message` - Chaîne de caractères à signer
    ///
    /// # Returns
    /// Une signature encodée (souvent en base64).
    fn sign(&self, message: &str) -> Result<String, SignError>;

    /// Vérifie qu’une signature correspond à un message.
    ///
    /// # Arguments
    /// * `message` - Le message original
    /// * `signature` - La signature encodée
    ///
    /// # Returns
    /// `true` si la signature est valide, `false` sinon.
    fn verify(&self, message: &str, signature: &str) -> Result<bool, VerifyError>;

    /// Construit une instance à partir d’une seed de 32+ octets.
    /// Généralement dérivée d'une phrase BIP-39.
    fn from_seed(seed: &[u8], mnemonic_words: Option<Vec<String>>) -> Result<Self, String>
    where
        Self: Sized;

    /// Retourne la clé privée encodée (ex: base64).
    fn encoded_private_key(&self) -> String;

    /// Retourne la clé publique encodée (ex: hex).
    fn encoded_public_key(&self) -> String;
}
