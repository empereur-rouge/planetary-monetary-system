//! pms-stockage/src/migrations.rs
//!
//! Mécanisme **simple** de migrations de schéma Redis pour le DAG.
//! - Version stockée sous la clé `pms:ver` (entier croissant).
//! - `ensure_schema()` lit la version et applique en chaîne les migrations manquantes.
//! - Chaque migration est **idempotente** (sûre à relancer).
//!
//! Bonnes pratiques :
//! - Incrémentez `CURRENT_VER` quand vous ajoutez une nouvelle étape.
//! - Ajoutez une fonction `mig_<n>_to_<n+1>()` et référencez‑la dans `ensure_schema()`.
//! - Visez des opérations atomiques/commutatives côté Redis (SADD/ZADD/HSET…), ou
//!   structurez votre code pour supporter les relances (idempotence).

use thiserror::Error;

/// Version logique du schéma attendu par ce binaire.
/// Incrémentez lorsqu'une nouvelle migration est introduite.
pub const CURRENT_VER: i64 = 12;

/// Version du protocole DAG (SemVer).
/// - MAJOR : changement incompatible (refus de démarrer, migration manuelle requise)
/// - MINOR : nouvelles fonctionnalités backward-compatible (migration auto)
/// - PATCH : correctifs (migration auto)
/// v3.0.0 (audit sécurité 2026-06-11) : règles de validation BREAKING —
/// les unlocks de transaction sont vérifiés (signature + ownership,
/// appariement input[i]↔unlock[i]) et le block id doit être le hash
/// canonique du contenu. Des blocs acceptés sous 2.x (unlocks invalides,
/// ids forgés) sont rejetés sous 3.x ; un re-sync depuis zéro peut refuser
/// un historique 2.x → wipe testnet requis.
/// v3.8.0 (semi-fongibles, demurrage) : champ additif `demurrage_bps_per_day` sur
/// `SftClass` (payload `SftClassCreate`) — une classe SFT peut décoter ses UTXO par
/// le même mécanisme que les tokens (protocole 2.5). Additif (serde default) → MINOR,
/// pas de wipe.
/// v3.7.0 (semi-fongibles, `pms-spec-semi-fungibles.md`) : nouvelle variante
/// `PlainPayload::SftClassCreate` (registre de classes SFT façon ERC-1155 ;
/// soldes portés par le moteur UTXO existant, `asset_id = "collection:class"`).
/// Additif : les blocs/CFs existants restent valides, nouveau CF `sft_classes`
/// (CURRENT_VER 11→12, auto-migrating). MINOR → pas de wipe. (Mixed-version P2P :
/// un nœud 3.6.0 ne sait pas désérialiser un `SftClassCreate`.)
/// v3.6.0 (plan §4 gouvernance, P3) : nouvelle variante `ConfigUpdate::SetEmissionCorridor`
/// (couloir d'émission gouverné — ceiling/floor/target/epoch en bps). Additive : un
/// nœud à jour parse le nouveau variant ; les anciens blocs/configs restent valides.
/// MINOR → migration auto, pas de wipe. (Mixed-version P2P : un nœud 3.5.0 ne sait pas
/// désérialiser un `SetEmissionCorridor`.)
/// v3.5.0 (plan §4 gouvernance, P2) : règles de validation des `GovernanceProposal`
/// renforcées — palier MINIMUM par paramètre (table param→tier) et asymétrie
/// tighten/loosen (`enact_after == announced_at + durée(direction)`). Une
/// proposition au palier trop bas ou au timelock incohérent est rejetée. Pas de
/// changement de format de bloc → MINOR, migration auto, pas de wipe. (Mixed-version
/// P2P : un nœud 3.5.0 valide plus strictement que 3.4.0.)
/// v3.4.0 (plan §4 gouvernance) : nouvelles variantes `PlainPayload::Governance{Proposal,Enact,Cancel}`
/// (changements de config timelockés + ancrés DAG). Additif backward-compatible →
/// migration auto, pas de wipe.
/// v3.3.0 (plan §3.1 voie B) : nouvelle variante `PlainPayload::TokenBurn`
/// (burn de token owner-signé, la supply baisse, trigger des contrats
/// `OnTokenBurn`). Backward-compatible : les blocs existants parsent toujours ;
/// un nœud à jour accepte le nouveau type. MINOR → migration auto, pas de wipe.
///
/// v3.9.0 (audit rang 2 — frais brûlés à la source) : la conservation du PMS
/// natif passe de l'égalité stricte (`in == out`) à `out ≤ in` — le frais de gas
/// est la différence `in − out`, détruite (modèle UTXO standard). Anciens blocs
/// (`in == out`) toujours valides ; nouveaux blocs (`in > out`) acceptés. MINOR
/// → migration auto, pas de wipe. (Mixed-version P2P : un nœud 3.8.0 REJETTE un
/// bloc 3.9.0 qui brûle un frais — mise à niveau coordonnée requise.)
///
/// v3.10.0 (audit rang 3 — B3) : anti-replay durable des `BridgeMint`. Chaque
/// `lock_block_id` (BridgeLock source) ne peut être minté QU'UNE FOIS sur le
/// ledger destination — marqueur durable dans la CF `bridge_consumed` écrit dans
/// le MÊME batch atomique que le bloc mint ; un BridgeMint rejouant un lock déjà
/// consommé est rejeté à la persistance. Règle additive : anciens blocs valides,
/// le 1er mint d'un lock toujours accepté. MINOR → migration auto, pas de wipe.
/// (Mixed-version P2P : un nœud 3.9.0 ré-appliquerait un BridgeMint rejoué — mise
/// à niveau coordonnée requise pour fermer l'inflation par replay.)
///
/// v3.11.0 (audit rang 3 — B3 réconciliation cross-ledger) : un `BridgeMint` est
/// réconcilié avec son `BridgeLock` source — montant total == verrouillé, asset
/// identique, destinataire == `dest_address` du lock, et le lock doit être
/// destiné à CE ledger. Un mint qui sur-émet (inflation), paie le mauvais
/// destinataire (vol), référence un lock inexistant, ou est appliqué sur le
/// mauvais ledger destination est rejeté. Règle additive : un mint légitime
/// (montant == lock) reste accepté. MINOR → migration auto, pas de wipe.
/// (Mixed-version P2P : un nœud 3.10.0 ne réconcilie pas les montants — upgrade
/// coordonné requis pour fermer l'inflation par mint mal formé.)
pub const DAG_VERSION: &str = "3.11.0";

/// Erreurs possibles lors des migrations.
#[derive(Error, Debug)]
pub enum MigError {
    /// Une version inconnue a été rencontrée (binaire trop ancien / données plus récentes).
    #[error("unexpected version {0}")]
    Unexpected(i64),
    /// Wrapper générique pour erreurs Anyhow.
    #[error(transparent)]
    Any(#[from] anyhow::Error),
}
