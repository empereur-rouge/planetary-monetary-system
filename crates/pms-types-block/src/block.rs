use pms_types_payload::{PayloadEnvelope, PlainPayload};
use serde::{Deserialize, Serialize};

pub type BlockId = String;

// ═══════════════════════════════════════════════════════════════════════════════
// BlockMetadata : Métadonnées optionnelles d'un block
// ═══════════════════════════════════════════════════════════════════════════════
//
// Ces métadonnées sont stockées avec le block mais NE SONT PAS incluses dans
// le calcul du BlockId (hash). Cela permet d'ajouter des informations lisibles
// sans affecter le consensus.
//
// Voir Chapitre 5 du Rust Book pour les structs avec des champs optionnels.
// ═══════════════════════════════════════════════════════════════════════════════

/// Métadonnées optionnelles d'un block (hors consensus)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlockMetadata {
    /// Description lisible humaine (ex: "Création supply initiale")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Tags libres pour catégorisation (ex: ["mint", "official"])
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Données additionnelles JSON (extensibilité future)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Block {
    pub id: BlockId,
    pub parents: Vec<String>, // 2..N
    pub payload: Option<PayloadEnvelope>,
    pub nonce: u64, // PoW léger (anti-spam)

    // Auth (Optionnel pour MVP, requis pour Milestones)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_pk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,

    // ═══════════════════════════════════════════════════════════════════════════
    // metadata : Champ optionnel pour les métadonnées
    // ═══════════════════════════════════════════════════════════════════════════
    //
    // - `#[serde(default)]` : Si absent lors de la désérialisation, devient None
    //   → Compatibilité backward avec les blocks existants sans metadata
    //
    // - `#[serde(skip_serializing_if = "Option::is_none")]` : N'est pas sérialisé
    //   si None → JSON plus compact
    // ═══════════════════════════════════════════════════════════════════════════
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BlockMetadata>,
}

impl Block {
    /// Crée un block "normal" (hors genesis).
    ///
    /// # Arguments
    /// - `parents` : IDs des blocks parents (au moins 1)
    /// - `payload` : Contenu du block (Mint, TxUtxo, etc.)
    /// - `nonce` : Preuve de travail anti-spam
    /// - `metadata` : Métadonnées optionnelles (description, tags)
    /// - `compute_id` : Fonction de calcul du hash/ID
    ///
    /// # Important
    /// `metadata` n'est PAS inclus dans `compute_id` pour la stabilité du consensus.
    pub fn new(
        parents: Vec<String>,
        payload: Option<PayloadEnvelope>,
        nonce: u64,
        metadata: Option<BlockMetadata>,
        compute_id: impl Fn(&[String], &Option<PayloadEnvelope>, u64) -> BlockId,
    ) -> Result<Self, String> {
        if parents.is_empty() {
            return Err("Un block hors genesis doit avoir au moins 1 parent".into());
        }
        // ⚠️ compute_id ne prend PAS metadata en paramètre → hash stable
        let id = compute_id(&parents, &payload, nonce);
        Ok(Self {
            id,
            parents,
            payload,
            nonce,
            metadata,
            signer_pk: None,
            signature: None,
        })
    }

    /// Crée le block genesis :
    /// - parents vides
    /// - payload = Genesis
    /// - nonce = 0
    /// - metadata = None
    pub fn genesis(
        compute_id: impl Fn(&[String], &Option<PayloadEnvelope>, u64) -> BlockId,
    ) -> Self {
        let parents = Vec::new();
        let payload = Some(PayloadEnvelope::Plain(PlainPayload::Genesis));
        let nonce = 0u64;
        let id = compute_id(&parents, &payload, nonce);
        Self {
            id,
            parents,
            payload,
            nonce,
            metadata: None,
            signer_pk: None,
            signature: None,
        }
    }
}
