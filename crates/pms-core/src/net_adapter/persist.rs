//! Block persistence pipeline for the DAG.
//!
//! Contains `do_persist_block()` -- the full validation -> persistence -> UTXO
//! pipeline extracted from the `NetDagAdapter` impl so the trait impl in
//! `mod.rs` can remain thin.

use super::helpers::plain_payload_type_str;
use crate::crypto::crypto::verify_block_signature;
use crate::validations::mint::validate_mint_security;
use crate::validations::nft::validate_nft_action;
use crate::validations::policy::validate_mint_policy;
use crate::CoreAdapter;
use anyhow::Result;
use num_traits::ToPrimitive;
use pms_event::PmsEvent;
use pms_storage::store::PutResult;
use pms_storage::coordinator_key_store::{CoordinatorKeyStorage, KeyRotationRecord};
use pms_storage::{
    ComplianceStorage, ConfigStorage, DagStorage, NftStorage, NodeRewardsStorage, StoredBlock,
    UtxoDelta,
};
use pms_types::{Block, PayloadEnvelope, PlainPayload};
use pms_wire::WireBlock;

impl<S> CoreAdapter<S>
where
    S: DagStorage
        + NftStorage
        + ConfigStorage
        + NodeRewardsStorage
        + ComplianceStorage
        + CoordinatorKeyStorage
        + Send
        + Sync
        + 'static,
{
    /// Full block persistence pipeline.
    ///
    /// Etapes:
    ///  0. Verification reseau + presence de la signature (niveau "auth" minimal).
    ///  1. Verifications legeres sur le `WireBlock` (taille payload, parents).
    ///  2. Persistance atomique en store (`StoredBlock`) -- source de verite.
    ///  3. Reconstruction du `Block` en RAM + mise a jour du DAG + finalite.
    ///  4. Persistance de la finalite (finals + dernier milestone) dans le store.
    /// Core persistence pipeline. `external_delta` is used for payloads the
    /// pipeline can't introspect (i.e. encrypted): the caller supplies the
    /// plaintext `UtxoDelta` so the spends are marked and the UTXO set is
    /// updated **inside the same critical section** as the block insert.
    /// Without this, the old `persist_block → apply_utxo_delta` sequence
    /// opened a race window where concurrent handlers could still see the
    /// inputs as spendable (audit finding H1).
    #[allow(clippy::too_many_lines)]
    pub(super) async fn do_persist_block_internal(
        &self,
        wb: &WireBlock,
        external_delta: Option<pms_storage::UtxoDelta>,
    ) -> Result<PutResult> {
        // ============================================================
        // 0) Use cached wire metadata (network_id + protocol_version).
        //    Avoids re-reading the config file on every block insertion.
        // ============================================================
        let meta = self.wire_meta.clone();
        let mut policy = self.policy.clone();

        // Charger la RuntimeConfig depuis le store (Hot-Swap)
        // et mettre a jour la policy avec les parametres dynamiques
        if let Ok(runtime_config) = self.store.get_runtime_config() {
            policy.update_from_runtime_config(&runtime_config);
            tracing::trace!(
                "RuntimeConfig applied: coordinator_fee={}bps, pow_bits={}",
                runtime_config.coordinator_fee_bps,
                runtime_config.min_pow_bits
            );
        }

        // AUDIT v0.9.0: l'autorité par payload (authority.rs, mint, NFT) suit
        // la clé coordinator COURANTE — un `CoordinatorKeyRotate` transfère
        // immédiatement l'autorité à new_pk (les anciennes clés en grace
        // window peuvent encore signer des blocs ordinaires via le
        // single-writer gate, mais plus exercer d'autorité). Appliqué une
        // seule fois ici sur la policy déjà clonée, pas de re-clone par bloc.
        if let Some(current_pk) = self
            .key_rotation_state
            .read()
            .current_pk()
            .map(|s| s.to_string())
        {
            policy.coordinator_public_key = Some(current_pk);
        }

        let policy = &policy;

        // ============================================================
        // 1) VALIDATION WIRE-LEVEL (header + signer + signature + PoW)
        // ============================================================

        // 1.a) Verifier le network_id / protocol_version
        if wb.network_id != meta.network_id || wb.protocol_version != meta.protocol_version as u16 {
            return Ok(PutResult::Rejected(
                "wrong network_id or protocol_version".to_string(),
            ));
        }

        // 1.b) Cle publique obligatoire
        if wb.signer_pk_hex.trim().is_empty() {
            return Ok(PutResult::Rejected("missing signer public key".to_string()));
        }

        // 1.c) Signature obligatoire
        if wb.signature_hex.trim().is_empty() {
            return Ok(PutResult::Rejected("missing signature".to_string()));
        }

        // 1.d) Verification cryptographique reelle
        if let Err(e) = verify_block_signature(wb) {
            return Ok(PutResult::Rejected(format!("invalid signature: {e}")));
        }

        // ============================================================
        // 1.e) SINGLE WRITER ENFORCEMENT (Private DAG Mode)
        // ============================================================
        // En mode Single Writer, TOUS les blocs doivent etre signes par le
        // Coordinator. v0.7.4 generalises this from "the bootstrap pk" to
        // "any pk currently in the active set" so a `CoordinatorKeyRotate`
        // block can hand authority over without restarting the network.
        // The active set is `current_pk` ∪ {old_pk's still in their grace
        // window}. When no rotation has ever landed it equals
        // `{bootstrap_pk}` so behaviour is identical to pre-0.7.4.
        let now_ms_for_signers = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let accepted_signer_keys = self
            .key_rotation_state
            .read()
            .accepted_signer_keys(now_ms_for_signers);
        // AUDIT H-4 (v0.9.0): posture FAIL-CLOSED. Avant, un active set vide
        // (clé bootstrap absente, état de rotation corrompu) SAUTAIT le
        // contrôle — n'importe quel bloc auto-signé était accepté. Désormais,
        // en Testnet/Mainnet, active set vide ⇒ rejet de TOUS les blocs
        // jusqu'à correction de la config. Seul le mode Dev pur (aucune clé
        // coordinator configurée, par design) reste permissif.
        if policy.enforce_single_writer {
            let is_dev_mode = matches!(self.settings.network.mode, pms_config::NetworkMode::Dev);
            if let Err(reason) =
                single_writer_gate(&accepted_signer_keys, &wb.signer_pk_hex, is_dev_mode)
            {
                tracing::warn!(
                    "🚫 Single Writer violation: block {} — {}",
                    &wb.id[..16.min(wb.id.len())],
                    reason
                );
                return Ok(PutResult::Rejected(format!("single_writer: {reason}")));
            }
        }

        // [DEPRECATED] 1.f) PoW check removed for Private DAG
        // Server authority replaces Proof-of-Work. The coordinator's signature
        // is the sole validation mechanism. PoW logic kept for documentation purposes.
        // See: validate_wire_block_pow() in core_adapter.rs (disabled, always returns Ok)

        // ============================================================
        // 2) DECODAGE PAYLOAD + CONTROLES STRUCTURELS SIMPLES
        //    (taille JSON, parents uniques, self-parent, min parents apres bootstrap)
        // ============================================================

        // 2.b) Taille maximale du payload brut (anti-spam)
        if let Some(s) = &wb.payload_json {
            if s.len() > policy.max_payload_bytes {
                return Ok(PutResult::Rejected(format!(
                    "payload too large: {} > {}",
                    s.len(),
                    policy.max_payload_bytes
                )));
            }
        }

        // 2.c) Deserialisation en PayloadEnvelope (si non-null)
        let payload: Option<PayloadEnvelope> = match &wb.payload_json {
            None => None,
            Some(s) if s.trim().is_empty() || s == "null" => None,
            Some(s) => Some(serde_json::from_str::<PayloadEnvelope>(s)?),
        };

        // 1.v) INTÉGRITÉ DE L'ID DE BLOC (audit M-6, v0.9.0)
        //
        // L'id sert de clé d'idempotence/déduplication (AlreadyExists) et de
        // référence parent. Avant ce check, un producteur autorisé pouvait
        // forger un id arbitraire (collision volontaire pour masquer/évincer
        // un bloc, ou id ne correspondant pas au contenu). On recalcule l'id
        // depuis le contenu canonique — parents + nonce + en-tête d'enveloppe
        // (commitment SHA-256 du payload) — et on rejette tout mismatch.
        // Indépendant du formatting JSON du client : le payload est
        // re-sérialisé sous forme canonique serde avant hachage.
        {
            let expected_id = pms_utils::compute_block_id(&wb.parents, &payload, wb.nonce);
            if wb.id != expected_id {
                tracing::warn!(
                    "🚫 Block id mismatch: declared {} != computed {} (signer {})",
                    &wb.id[..16.min(wb.id.len())],
                    &expected_id[..16],
                    &wb.signer_pk_hex[..16.min(wb.signer_pk_hex.len())]
                );
                return Ok(PutResult::Rejected(
                    "block id does not match canonical content hash".to_string(),
                ));
            }
        }

        // 1.w) AUTORITÉ PAR TYPE DE PAYLOAD (audit C-2 extension, v0.9.0)
        //
        // Les checks coordinator-only (Milestone, ConfigUpdate, Reward,
        // TokenCreate, Bridge*, Freeze/Seize/Reverse, Contract*, etc.)
        // vivaient dans le validate_block legacy, retiré du hot path —
        // ils n'étaient donc plus appliqués qu'à travers l'enforcement
        // single-writer. On les ré-applique ICI, AVANT tout apply d'état.
        // La policy porte déjà la clé COURANTE (override rotation en tête
        // de fonction).
        if let Err(e) = crate::validations::authority::validate_payload_authority(
            Some(wb.signer_pk_hex.as_str()),
            payload.as_ref(),
            policy,
        ) {
            tracing::warn!(
                "🚫 Payload authority violation on block {}: {e}",
                &wb.id[..16.min(wb.id.len())]
            );
            return Ok(PutResult::Rejected(format!("payload authority: {e}")));
        }

        // 1.x) Politique de mint (PlainPayload::Mint seulement)
        //
        // - Plain + Mint = visible -> on peut appliquer les regles de montant et d'admin.
        // - EncryptedPayload::Mint reste pour l'instant traite comme "opaque",
        //   la politique de mint ne peut pas etre appliquee dessus.
        if let Some(PayloadEnvelope::Plain(PlainPayload::Mint { outputs })) = &payload {
            // Verification 1: Montant et politique generale
            if let Err(e) = validate_mint_policy(outputs, wb, &self.settings) {
                return Ok(PutResult::Rejected(format!("mint policy violated: {e}")));
            }

            // Verification 1.b: Spend conditions (protocole 2.2) — un Mint
            // peut créer des outputs time-lockés / multisig / hashlock, mais
            // leurs conditions doivent être bien formées (adresse multisig
            // canonique, hash SHA-256 valide, M ≤ N, etc.).
            if let Err(e) = crate::validations::conditions::validate_output_conditions(outputs) {
                return Ok(PutResult::Rejected(format!("mint output condition: {e}")));
            }

            // Verification 2: SECURITE COORDINATEUR
            // Seul le Coordinateur peut minter (Mainnet/Testnet).
            //
            // v0.7.4: mint authority follows the **current** rotation
            // pointer, not the bootstrap key. So a `CoordinatorKeyRotate`
            // block atomically transfers the right to mint to `new_pk` —
            // old keys still in the SINGLE_WRITER grace window can sign
            // chain blocks but cannot mint. This is the conservative
            // choice for back-to-back rotations: mint is the most
            // sensitive authority, so we narrow it the moment the new
            // key is announced.
            //
            // v0.9.0: réutilise `policy` (clé bootstrap résolue par
            // ValidatePolicy::from_global_config + override rotation appliqué
            // en tête de fonction) — même sémantique, sans re-dérivation.
            if let Err(e) = validate_mint_security(wb, policy) {
                tracing::warn!(
                    "🚫 Unauthorized mint attempt blocked: {} from signer {}",
                    wb.id,
                    &wb.signer_pk_hex[..16.min(wb.signer_pk_hex.len())]
                );
                return Ok(PutResult::Rejected(format!(
                    "mint security: {}. Key: {:?}",
                    e, policy.coordinator_public_key
                )));
            }
        }

        // 1.y) Validation NFT (PlainPayload::Nft)
        //
        // - Valide l'action NFT (ownership, existence, autorisation)
        // - Applique au store si valide (Mint -> apply_mint, Transfer/Burn -> apply_action)
        if let Some(PayloadEnvelope::Plain(PlainPayload::Nft(action))) = &payload {
            let signer_pk = &wb.signer_pk_hex;

            if let Err(e) = validate_nft_action(
                action,
                signer_pk,
                policy.coordinator_public_key.as_deref(),
                self.store.as_ref(),
            ) {
                tracing::warn!(
                    "🚫 NFT action rejected: {} - token: {}",
                    e,
                    action.token_id()
                );
                return Ok(PutResult::Rejected(format!("NFT validation: {}", e)));
            }

            // Applique l'action (modifie ownership)
            // Privacy: Pour Mint, on utilise apply_mint avec block_id
            // Privacy: Pour Transfer avec re-encryption, on utilise apply_transfer
            use pms_types_nft::NftAction;
            let apply_result = match action {
                NftAction::Mint {
                    token_id, creator, ..
                } => {
                    // Privacy-first: stocke (token_id, owner, block_id) - pas de metadata
                    self.store.apply_mint(token_id, creator, &wb.id)
                }
                NftAction::Transfer {
                    token_id,
                    to,
                    new_owner_x25519_pubkey,
                    ..
                } => {
                    // Si new_owner_x25519_pubkey est fourni, on met a jour le block_id
                    // pour pointer vers ce bloc Transfer (qui contiendra les metadonnees
                    // re-chiffrees pour le nouveau owner via le serveur coordinator)
                    if new_owner_x25519_pubkey.is_some() {
                        // Re-encryption: le bloc Transfer devient la nouvelle reference
                        self.store.apply_transfer(token_id, to, &wb.id)
                    } else {
                        // Transfer simple sans re-encryption (ancien owner garde acces)
                        self.store.set_owner(token_id, to)
                    }
                }
                _ => {
                    // Burn, Use, BatchBurn
                    self.store.apply_action(action)
                }
            };

            if let Err(e) = apply_result {
                tracing::error!("❌ NFT apply failed: {}", e);
                return Ok(PutResult::Rejected(format!("NFT apply failed: {}", e)));
            }

            // Emet l'evenement NFT sur le bus
            self.event_bus
                .emit(PmsEvent::nft(wb.id.clone(), action.clone()));

            tracing::info!(
                "✅ NFT action applied: {} - token: {}",
                action.action_type_str(),
                action.token_id()
            );
        }

        // 1.z) Validation ConfigUpdate (PlainPayload::ConfigUpdate)
        //
        // - Applique la mise a jour de configuration
        // - Persiste dans le store
        // - Emet un evenement
        if let Some(PayloadEnvelope::Plain(PlainPayload::ConfigUpdate(update))) = &payload {
            // Timestamp actuel
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);

            // Appliquer la mise a jour au store
            match self.store.apply_config_update(update, &wb.id, timestamp) {
                Ok(new_config) => {
                    tracing::info!(
                        "✅ Config update applied: {} - fee_rate={}bps, coordinator={}bps, treasury={}bps",
                        update.description(),
                        new_config.fee_rate_bps,
                        new_config.coordinator_fee_bps,
                        new_config.treasury_fee_bps
                    );
                }
                Err(e) => {
                    tracing::error!("❌ ConfigUpdate apply failed: {}", e);
                    return Ok(PutResult::Rejected(format!("ConfigUpdate failed: {}", e)));
                }
            }
        }

        // 1.compliance) Apply compliance registry operations (Freeze / Unfreeze / Seize / Reverse)
        if let Some(PayloadEnvelope::Plain(PlainPayload::Freeze {
            ref address,
            ref reason,
        })) = payload
        {
            if let Err(e) = self.store.freeze_address(address, &wb.id, reason) {
                return Ok(PutResult::Rejected(format!("Freeze failed: {e}")));
            }
            tracing::info!("🔒 Account frozen: {} (block {})", address, wb.id);
        }
        if let Some(PayloadEnvelope::Plain(PlainPayload::Unfreeze {
            ref address,
            ref reason,
            ..
        })) = payload
        {
            if let Err(e) = self.store.unfreeze_address(address) {
                return Ok(PutResult::Rejected(format!("Unfreeze failed: {e}")));
            }
            if let Err(e) = self.store.log_compliance_action(
                "unfreeze",
                &wb.id,
                Some(address),
                &serde_json::json!({ "reason": reason }),
            ) {
                tracing::warn!("Compliance log failed: {e}");
            }
            tracing::info!("🔓 Account unfrozen: {} (block {})", address, wb.id);
        }
        if let Some(PayloadEnvelope::Plain(PlainPayload::Seize {
            ref from_address,
            ref reason,
            ..
        })) = payload
        {
            if let Err(e) = self.store.log_compliance_action(
                "seize",
                &wb.id,
                Some(from_address),
                &serde_json::json!({ "reason": reason }),
            ) {
                tracing::warn!("Compliance log failed: {e}");
            }
            tracing::info!("⚖️ Assets seized from: {} (block {})", from_address, wb.id);
        }
        if let Some(PayloadEnvelope::Plain(PlainPayload::Reverse {
            ref original_block_id,
            ref reason,
            ..
        })) = payload
        {
            if let Err(e) = self.store.log_compliance_action(
                "reverse",
                &wb.id,
                None,
                &serde_json::json!({ "reason": reason, "original_block_id": original_block_id }),
            ) {
                tracing::warn!("Compliance log failed: {e}");
            }
            tracing::info!(
                "↩️ Transaction reversed: {} (block {})",
                original_block_id,
                wb.id
            );
        }

        // 1.rotation) CoordinatorKeyRotate validation + record (audit
        // item 8, v0.7.4). The block must be signed by the **current**
        // coordinator key (not just any key in the grace window) and
        // its `old_pk` field must match that current key — this
        // prevents an attacker holding a still-in-grace old_pk from
        // chaining a fresh rotation to keep authority indefinitely.
        if let Some(PayloadEnvelope::Plain(PlainPayload::CoordinatorKeyRotate {
            ref old_pk,
            ref new_pk,
            grace_window_seconds,
        })) = payload
        {
            let current_pk = self
                .key_rotation_state
                .read()
                .current_pk()
                .map(|s| s.to_string());
            let signer = wb.signer_pk_hex.trim().to_string();
            let declared_old = old_pk.trim().to_string();

            // Reject if no current pk is known (pure dev mode without
            // a coordinator key configured) — rotation only makes sense
            // when a key was bootstrapped.
            let Some(current) = current_pk else {
                return Ok(PutResult::Rejected(
                    "CoordinatorKeyRotate: no bootstrap coordinator key configured; rotation requires an initial key".into(),
                ));
            };
            if !signer.eq_ignore_ascii_case(&current) {
                return Ok(PutResult::Rejected(format!(
                    "CoordinatorKeyRotate: must be signed by the current coordinator key {} (got {})",
                    current, signer
                )));
            }
            if !declared_old.eq_ignore_ascii_case(&current) {
                return Ok(PutResult::Rejected(format!(
                    "CoordinatorKeyRotate: old_pk field {} does not match current coordinator key {}",
                    declared_old, current
                )));
            }
            if new_pk.trim().is_empty() || new_pk.trim().eq_ignore_ascii_case(&current) {
                return Ok(PutResult::Rejected(
                    "CoordinatorKeyRotate: new_pk must be non-empty and differ from current".into(),
                ));
            }

            // Persist the rotation. We do this BEFORE inserting the
            // block into the DAG so a failure here aborts the persist
            // entirely — the block will be rejected, never half-applied.
            let record = KeyRotationRecord {
                old_pk: current.clone(),
                new_pk: new_pk.trim().to_string(),
                applied_at_block_id: wb.id.clone(),
                applied_at_ts_ms: now_ms_for_signers,
                grace_window_seconds,
            };
            if let Err(e) = self.store.record_key_rotation(&record) {
                tracing::error!(
                    target = "key_rotation",
                    error = %e,
                    "Failed to persist key rotation record — block REJECTED"
                );
                return Ok(PutResult::Rejected(format!(
                    "CoordinatorKeyRotate: failed to persist history: {e}"
                )));
            }
            // Refresh the in-RAM cache so subsequent blocks in this
            // process see the new authority immediately.
            self.refresh_key_rotation_state();
            tracing::warn!(
                target = "key_rotation",
                block_id = %wb.id,
                old_pk = %record.old_pk,
                new_pk = %record.new_pk,
                grace_window_seconds,
                "🔑 Coordinator key rotated"
            );
        }

        // 2.d) Parents uniques + pas d'auto-parentage (protection de base)
        {
            use std::collections::HashSet;
            let mut seen = HashSet::new();

            if !wb.parents.iter().all(|p| seen.insert(p)) {
                return Ok(PutResult::Rejected("duplicate parent reference".into()));
            }

            if wb.parents.iter().any(|p| p == &wb.id) {
                return Ok(PutResult::Rejected("self-parent not allowed".into()));
            }
        }

        // 2.e) Minimum de parents apres bootstrap (soft anti-spam)
        //
        // On requiert min_parents (typiquement 2),
        // MAIS on permet d'utiliser "genesis" comme parent supplementaire
        // si le DAG n'a pas assez de tips distincts.
        //
        // NOTE: En mode Single Writer, cette regle est REMPLACEE par enforce_single_parent.
        //
        // Regle:
        //  - parents.len() >= min_parents_after_boot
        //  - SAUF si le bloc contient "genesis" comme parent ET qu'il n'y a pas assez de tips
        //  - Dans ce cas, genesis peut "completer" le compte de parents
        if !policy.enforce_single_writer {
            let dag_was_bootstrapped = { self.dag.len() > 1 };
            if dag_was_bootstrapped && wb.parents.len() < policy.min_parents_after_boot {
                // Verifier si genesis est utilise comme parent supplementaire
                let has_genesis = wb.parents.iter().any(|p| p == "genesis");
                let available_tips = self.dag.find_tips().len();

                // Autoriser si genesis est utilise ET qu'il n'y a pas assez de tips disponibles
                if !(has_genesis && available_tips < policy.min_parents_after_boot) {
                    return Ok(PutResult::Rejected(format!(
                        "not enough parents after bootstrap: got {}, need {}. Tip: use 'genesis' as parent during bootstrap.",
                        wb.parents.len(),
                        policy.min_parents_after_boot
                    )));
                }
            }
        }

        // 2.f) SINGLE WRITER: Chaine Lineaire (1 parent)
        //
        // En mode Single Writer, on impose exactement 1 parent par bloc.
        // Cela garantit une chaine lineaire au lieu d'un DAG.
        if self.settings.validation.enforce_single_writer {
            let is_genesis = payload
                .as_ref()
                .is_some_and(|p| matches!(p, PayloadEnvelope::Plain(PlainPayload::Genesis)));

            // Genesis: 0 parents, Non-genesis: exactement 1 parent
            if !is_genesis && wb.parents.len() != 1 {
                tracing::warn!(
                    "🚫 Single Writer violation: block {} has {} parents (expected 1)",
                    &wb.id[..16.min(wb.id.len())],
                    wb.parents.len()
                );
                return Ok(PutResult::Rejected(format!(
                    "single_writer: block must have exactly 1 parent, got {}",
                    wb.parents.len()
                )));
            }
        }

        // ============================================================
        // 3) RECONSTRUCTION DU Block (objet RAM) POUR LA VALIDATION DAG
        // ============================================================
        //
        // A ce stade:
        //   - header ok (reseau, signature, PoW)
        //   - payload JSON parse (ou None)
        //   - contraintes structurelles simples faites (taille, parents uniques)
        //
        // On peut donc construire un Block propre et coherent.
        let block = Block {
            id: wb.id.clone(),
            parents: wb.parents.clone(),
            payload: payload.clone(),
            nonce: wb.nonce,
            metadata: None, // Les blocs recus du reseau n'ont pas de metadata
            signer_pk: Some(wb.signer_pk_hex.clone()).filter(|s| !s.is_empty()),
            signature: Some(wb.signature_hex.clone()).filter(|s| !s.is_empty()),
        };

        // ============================================================
        // 4) VALIDATION DAG PROFONDE (UTXO, double-spend, regles metier)
        // ============================================================
        //
        // On utilise ta fonction `validate_block(dag, &block, policy)` qui:
        //   - verifie parents_exist / no_cycle / parent_count
        //   - applique la politique UTXO (double spend, montants, etc.)
        //
        // Important: on ne modifie pas le DAG ici, on fait juste les checks.
        //
        // ## FIX RACE CONDITION (Phase 2 IOTA-like)
        //
        // Les tips sont selectionnes depuis RocksDB (`store.top_tips()`), mais
        // validate_block verifie les parents en RAM. Sous charge parallele,
        // un parent peut exister dans RocksDB mais pas encore en RAM.
        //
        // Solution: verifier d'abord que les parents existent dans le store.
        // ============================================================

        // 4.a) Verification des parents (RAM DAG + store)
        // FIX: Check RAM DAG first to handle async persistence race condition
        let t_parents_start = std::time::Instant::now();
        if policy.enforce_parent_existence {
            for parent_id in &block.parents {
                // Check RAM DAG first (blocks are inserted here immediately)
                let in_ram = self.dag.contains_block(parent_id);

                // If not in RAM, check store (for blocks not yet loaded in RAM)
                if !in_ram {
                    match self.store.get_block(parent_id).await {
                        Ok(Some(_)) => continue, // Parent in store
                        Ok(None) | Err(_) => {
                            return Ok(PutResult::Rejected(format!(
                                "dag validation failed: parent {} not found",
                                parent_id
                            )));
                        }
                    }
                }
            }
        }
        let t_parents = t_parents_start.elapsed();

        // 4.new) Validation UTXO Async (Sharding Phase 4)
        //
        // AUDIT C-1/C-2/H-3 (v0.9.0): la validation est INCONDITIONNELLE —
        // elle ne dépend plus de `policy.skip_utxo_checks` (ce flag ne pilote
        // plus que le chemin sync legacy de `validate_block`). Le hot path
        // vérifie désormais l'AUTORISATION complète de la dépense :
        //   - signatures de transaction (unlocks) sur le message canonique
        //     `{network_id, inputs, outputs, fee}` (C-2),
        //   - binding pubkey ↔ adresse propriétaire de chaque UTXO dépensé (C-1),
        //   - appariement strict input[i] ↔ unlock[i],
        //   - fee sanity + conservation stricte par asset (M-7),
        //   - existence des inputs + anti double-spend.
        let t_utxo_val_start = std::time::Instant::now();
        if let Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx))) = &block.payload {
            use crate::validations::transactions::validate_transaction_full;
            let tx_input_outputs = match validate_transaction_full(
                &self.utxos,
                tx,
                policy,
                now_ms_for_signers.max(0) as u64,
            )
            .await
            {
                Ok(outs) => outs,
                Err(e) => {
                    return Ok(PutResult::Rejected(format!("utxo validation failed: {e}")));
                }
            };

            // 4.compliance) Freeze check: reject transactions involving frozen
            // addresses. Reuses the input outputs fetched during validation
            // (no second ShardedUtxoSet lookup).
            for out in &tx_input_outputs {
                if self.store.is_frozen(&out.address).unwrap_or(false) {
                    return Ok(PutResult::Rejected(format!(
                        "compliance: sender address is frozen: {}",
                        out.address
                    )));
                }
            }
            for out in &tx.outputs {
                if self.store.is_frozen(&out.address).unwrap_or(false) {
                    return Ok(PutResult::Rejected(format!(
                        "compliance: recipient address is frozen: {}",
                        out.address
                    )));
                }
            }
        }
        if let Some(PayloadEnvelope::Plain(PlainPayload::BridgeLock {
            inputs,
            amount,
            asset_id,
            ..
        })) = &block.payload
        {
            use crate::validations::transactions::validate_bridge_lock_async;
            if let Err(e) = validate_bridge_lock_async(&self.utxos, inputs, amount, asset_id).await
            {
                return Ok(PutResult::Rejected(format!(
                    "bridge lock utxo validation failed: {e}"
                )));
            }
        }
        if let Some(PayloadEnvelope::Plain(PlainPayload::BridgeLock { inputs, .. })) =
            &block.payload
        {
            for inp in inputs {
                if let Some(out) = self.utxos.get(&inp.out).await {
                    if self.store.is_frozen(&out.address).unwrap_or(false) {
                        return Ok(PutResult::Rejected(format!(
                            "compliance: sender address is frozen: {}",
                            out.address
                        )));
                    }
                }
            }
        }

        let t_utxo_val = t_utxo_val_start.elapsed();

        // 4.b) DAG validation is handled by the lock-free pipeline:
        //   - Parent existence: checked in step 4.a (RAM DAG + store)
        //   - Double-spend / UTXO: checked in step 4.new (ShardedUtxoSet)
        //   - Structural checks: steps 2.d, 2.e, 2.f above
        // The legacy locked validate_block() was removed as it caused 27-280ms latency.
        let t_dag_val_start = std::time::Instant::now();
        let t_dag_val = t_dag_val_start.elapsed();

        // ============================================================
        // 5) PERSISTENCE ATOMIQUE EN ROCKSDB
        // ============================================================
        //
        // On convertit le WireBlock en StoredBlock (format de stockage), et on
        // utilise append_block_atomic() pour garantir:
        //   - idempotence (AlreadyExists si bloc deja present),
        //   - mise a jour coherente des index (tips, children_count, timestamps).
        let sb = StoredBlock {
            id: wb.id.clone(),
            parents: wb.parents.clone(),
            payload_json: wb.payload_json.clone(), // deferred clone -- rejected blocks skip this
            nonce: wb.nonce,
            network_id: wb.network_id.clone(),
            protocol_version: wb.protocol_version,
            signer_pk_hex: wb.signer_pk_hex.clone(),
            signature_hex: wb.signature_hex.clone(),
            metadata: wb.metadata.clone(),
        };

        // UTXO delta: callers of `persist_block_with_delta` supply the
        // plaintext view of an encrypted transaction here. If present, it
        // takes precedence over any delta the pipeline could have derived
        // from the payload — and it MUST, because for encrypted payloads
        // the pipeline can't see the plaintext at all (`_ => None` branch
        // below). Applying the caller's delta inside the same critical
        // section as the block insert closes the H1 race.
        let delta = if let Some(d) = external_delta {
            Some(d)
        } else {
            match &payload {
            Some(PayloadEnvelope::Plain(PlainPayload::Mint { outputs })) => {
                // Mint = create only (no inputs)
                let create = outputs
                    .iter()
                    .enumerate()
                    .map(|(i, out)| (sb.id.clone(), i as u32, out.clone()))
                    .collect();

                Some(UtxoDelta {
                    spend: vec![],
                    create,
                })
            }

            Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx))) => {
                // Tx = spend inputs + create outputs
                let spend = tx
                    .inputs
                    .iter()
                    .map(|inp| (inp.out.txid.clone(), inp.out.index))
                    .collect();

                let create = tx
                    .outputs
                    .iter()
                    .enumerate()
                    .map(|(i, out)| (sb.id.clone(), i as u32, out.clone()))
                    .collect();

                // Accumulation du pool de fees pour le Treasury
                // Calcul: fee * (treasury_fee_bps / 10000)
                if let Ok(runtime_config) = self.store.get_runtime_config() {
                    if runtime_config.treasury_fee_bps > 0 {
                        // Parse fee (format decimal: "1.50000000")
                        if let Ok(fee_decimal) = rust_decimal::Decimal::from_str_exact(&tx.fee) {
                            // Convertir en satoshis (8 decimales) with overflow protection
                            let fee_sats = match (fee_decimal
                                * rust_decimal::Decimal::from(100_000_000))
                            .to_u64()
                            {
                                Some(v) => v,
                                None => {
                                    tracing::error!(
                                        "Fee overflow: {} exceeds u64 range, capping",
                                        fee_decimal
                                    );
                                    u64::MAX
                                }
                            };

                            // Part pour le Treasury (use u128 intermediate to prevent overflow)
                            let treasury_portion = ((fee_sats as u128)
                                * u128::from(runtime_config.treasury_fee_bps)
                                / 10000) as u64;

                            if treasury_portion > 0 {
                                if let Err(e) = self.store.add_to_fee_pool(treasury_portion) {
                                    tracing::warn!("Failed to add to fee pool: {}", e);
                                }
                            }
                        }
                    }
                }

                Some(UtxoDelta { spend, create })
            }

            Some(PayloadEnvelope::Plain(PlainPayload::Reward {
                fee_outputs,
                reward_outputs,
                ..
            })) => {
                // Reward = create outputs for fee distribution + block rewards (no inputs)
                // fee_outputs: treasury, creator, parent signers
                // reward_outputs: creator, treasury
                let mut create = Vec::new();
                let mut idx = 0u32;

                // Add fee distribution outputs (always PMS native — asset_id forcé à None)
                for out in fee_outputs {
                    create.push((
                        sb.id.clone(),
                        idx,
                        pms_types::TxOutput {
                            asset_id: None,
                            ..out.clone()
                        },
                    ));
                    idx += 1;
                }

                // Add block reward outputs (always PMS native — asset_id forcé à None)
                for out in reward_outputs {
                    create.push((
                        sb.id.clone(),
                        idx,
                        pms_types::TxOutput {
                            asset_id: None,
                            ..out.clone()
                        },
                    ));
                    idx += 1;
                }

                if create.is_empty() {
                    None
                } else {
                    Some(UtxoDelta {
                        spend: vec![],
                        create,
                    })
                }
            }

            Some(PayloadEnvelope::Plain(PlainPayload::BridgeLock { inputs, .. })) => {
                // BridgeLock = spend inputs, create nothing (funds leave this ledger)
                let spend = inputs
                    .iter()
                    .map(|inp| (inp.out.txid.clone(), inp.out.index))
                    .collect();
                Some(UtxoDelta {
                    spend,
                    create: vec![],
                })
            }

            Some(PayloadEnvelope::Plain(PlainPayload::BridgeMint { outputs, .. })) => {
                // BridgeMint = create outputs, spend nothing (funds arrive on this ledger)
                let create = outputs
                    .iter()
                    .enumerate()
                    .map(|(i, out)| (sb.id.clone(), i as u32, out.clone()))
                    .collect();
                Some(UtxoDelta {
                    spend: vec![],
                    create,
                })
            }

            // Compliance: Seize and Reverse have UTXO deltas (spend + create)
            Some(PayloadEnvelope::Plain(PlainPayload::Seize {
                inputs, outputs, ..
            })) => {
                let spend = inputs
                    .iter()
                    .map(|inp| (inp.out.txid.clone(), inp.out.index))
                    .collect();
                let create = outputs
                    .iter()
                    .enumerate()
                    .map(|(i, out)| (sb.id.clone(), i as u32, out.clone()))
                    .collect();
                Some(UtxoDelta { spend, create })
            }
            Some(PayloadEnvelope::Plain(PlainPayload::Reverse {
                inputs, outputs, ..
            })) => {
                let spend = inputs
                    .iter()
                    .map(|inp| (inp.out.txid.clone(), inp.out.index))
                    .collect();
                let create = outputs
                    .iter()
                    .enumerate()
                    .map(|(i, out)| (sb.id.clone(), i as u32, out.clone()))
                    .collect();
                Some(UtxoDelta { spend, create })
            }
                // Freeze/Unfreeze: no UTXO changes (registry-only)
                _ => None,
            }
        };

        // Note: append_block_atomic_with_utxo prend Option<&UtxoDelta>
        // === ASYNC PERSISTENCE: Update RAM first, persist in background ===

        let t0 = std::time::Instant::now();

        // 5.a) UTXO RAM Update FIRST (essential for preventing double-spend)
        //
        // Uses apply_diff() which groups operations by shard for minimal lock
        // acquisitions instead of sequential per-UTXO awaits.
        if let Some(d) = &delta {
            let spends: Vec<pms_types::OutputId> = d
                .spend
                .iter()
                .map(|(txid, idx)| pms_types::OutputId {
                    txid: txid.clone(),
                    index: *idx,
                })
                .collect();
            let creates: Vec<(pms_types::OutputId, pms_types::TxOutput)> = d
                .create
                .iter()
                .map(|(txid, idx, out)| {
                    (
                        pms_types::OutputId {
                            txid: txid.clone(),
                            index: *idx,
                        },
                        out.clone(),
                    )
                })
                .collect();
            self.utxos.apply_diff(&spends, &creates).await;
        }

        let t_utxo = t0.elapsed();

        // ============================================================
        // 5.b) Insert into ConcurrentDag (LOCK-FREE, IOTA-like)
        // ============================================================
        let t1 = std::time::Instant::now();

        // Check if already exists
        if self.dag.contains_block(&sb.id) {
            return Ok(PutResult::AlreadyExists);
        }

        // Extract the block id once — reused by finality bookkeeping, the
        // EventBus emit path, and per-block log lines. Lets us `move` the
        // full `block` and `sb` structs into their consumers below without
        // paying for another full clone on every call.
        let block_id: String = sb.id.clone();

        // Lock-free insertion into concurrent DAG (block is moved in, not cloned).
        self.dag.insert_block(block);

        // Snapshot the post-insert children_count for each parent. The
        // consumer-side `append_blocks_batch` used to re-read these from
        // RocksDB on every batch — the multi_get_cf walk became the
        // dominant TPS-degradation cost as the DAG grew (parent blocks
        // age out of the memtable). Reading here is a lock-free
        // `AtomicU64::load`; the consumer writes the value verbatim,
        // and FIFO channel order guarantees the latest value persists.
        let parent_count_updates: Vec<(String, u64)> = sb
            .parents
            .iter()
            .map(|p| (p.clone(), self.dag.get_children_count(p)))
            .collect();

        // Mark spent outpoints in concurrent DAG (for double-spend detection)
        if let Some(d) = &delta {
            for (txid, idx) in &d.spend {
                self.dag.mark_spent(txid, *idx);
            }
        }

        // ============================================================
        // 6) FINALITY UPDATE (Milestone + k-depth)
        // ============================================================
        //
        // a) If this is a Milestone block, update last_milestone and mark it final
        // b) Run k-depth finalization for all blocks
        let mut newly_finalized: Vec<String> = Vec::new();
        // Collect reward UTXOs to add (done after lock to avoid async in lock)
        let mut reward_utxos: Vec<(pms_types::OutputId, pms_types::TxOutput, String, u64)> =
            Vec::new();
        {
            let mut finality = self.dag.finality.write();

            // a) Milestone handling
            if let Some(PayloadEnvelope::Plain(PlainPayload::Milestone {
                distribute_node_rewards,
                ..
            })) = &payload
            {
                finality.last_milestone = Some(block_id.clone());
                if finality.finalized.insert(block_id.clone()) {
                    newly_finalized.push(block_id.clone());
                }

                // Distribution des fees aux noeuds si demande
                if *distribute_node_rewards {
                    if let Ok(pool) = self.store.get_fee_pool() {
                        if pool > 0 {
                            if let Ok(miners) = self.store.get_all_miners() {
                                let total_blocks: u64 = miners.iter().map(|(_, c)| *c).sum();
                                if total_blocks > 0 {
                                    tracing::info!(
                                        "📤 Distributing {} sats to {} miners (total_blocks={})",
                                        pool,
                                        miners.len(),
                                        total_blocks
                                    );

                                    // Collecter les UTXOs a creer (sans await)
                                    for (idx, (node_pk, block_count)) in miners.iter().enumerate() {
                                        let share = pool * block_count / total_blocks;
                                        if share > 0 {
                                            let reward_address = self
                                                .store
                                                .get_node_reward_address(node_pk)
                                                .unwrap_or_else(|_| node_pk.clone());

                                            let amount = rust_decimal::Decimal::from(share)
                                                / rust_decimal::Decimal::from(100_000_000);
                                            let amount_str = format!("{:.8}", amount);

                                            let out_id = pms_types::OutputId {
                                                txid: block_id.clone(),
                                                index: idx as u32,
                                            };
                                            // rewards always PMS native
                                            let out = pms_types::TxOutput::new(
                                                reward_address.clone(),
                                                amount_str,
                                                None,
                                            );

                                            reward_utxos.push((
                                                out_id,
                                                out,
                                                node_pk.clone(),
                                                share,
                                            ));

                                            tracing::info!(
                                                "  → {} blocks = {} sats → {}",
                                                block_count,
                                                share,
                                                &reward_address[..20.min(reward_address.len())]
                                            );
                                        }
                                    }

                                    // Reset le pool et les compteurs
                                    if let Err(e) = self.store.reset_pool_and_counts() {
                                        tracing::error!("Failed to reset pool: {}", e);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // b) K-depth finalization (INCREMENTAL)
            //
            // Instead of scanning ALL blocks in the DAG (O(N * BFS) per insert),
            // we only check ancestors of the newly inserted block. These are the
            // only blocks that could have gained a new descendant (the new block
            // itself or its subtree). This reduces the cost from O(N * k) to O(k^2).
            let depth_k = finality.depth_k;
            if depth_k > 0 {
                let ancestors = self.dag.ancestors_within_depth(&block_id, depth_k);

                for ancestor_id in ancestors {
                    if finality.finalized.contains(&ancestor_id) {
                        continue;
                    }

                    let confirmations = self.dag.count_descendants(&ancestor_id, depth_k);
                    if confirmations >= depth_k {
                        finality.finalized.insert(ancestor_id.clone());
                        newly_finalized.push(ancestor_id);
                    }
                }
            }
        }

        let t_dag = t1.elapsed();

        // ============================================================
        // 6.c) Creer les UTXOs de recompense (apres le lock finality)
        // ============================================================
        for (out_id, out, node_pk, share) in reward_utxos {
            // Ajouter a UTXO RAM (async est ok ici, hors du lock)
            self.utxos.add(out_id, out.clone()).await;

            // Emettre un evenement
            self.event_bus.emit(PmsEvent::NodeRewardDistributed {
                node_pk,
                address: out.address,
                amount_sats: share,
                milestone_id: sb.id.clone(),
            });
        }

        // ============================================================
        // 7) BACK-PRESSURE: Send to background persist channel
        // ============================================================
        // A financial system must NEVER silently drop blocks. We block on
        // `send().await` without a timeout so a saturated persist pipeline
        // propagates back-pressure all the way to the HTTP caller instead of
        // producing a fake `Inserted` acknowledgement for a block that was
        // never queued to RocksDB.
        //
        // While blocked we log every second so a stall is visible in logs.
        // A closed channel means the background task has died — that is a
        // fatal, unrecoverable condition for this request, so we return an
        // error instead of pretending the block was persisted.
        use crate::background_persist::PersistJob;
        // `sb` is moved into the job — previously we paid for a full
        // `StoredBlock` clone here (including the payload_json, which can be
        // 10-100 KB for encrypted blocks). The pre-extracted `block_id` above
        // covers everything that still needs a stable string reference below.
        let payload_json_for_event = sb.payload_json.clone().unwrap_or_default();
        let job = PersistJob {
            block: sb,
            delta,
            newly_finalized,
            parent_count_updates,
        };
        let send_start = std::time::Instant::now();
        {
            let send_fut = self.persist_tx.send(job);
            tokio::pin!(send_fut);

            let mut warn_interval =
                tokio::time::interval(std::time::Duration::from_secs(1));
            // `interval`'s first tick fires immediately; consume it so warnings
            // only start after ~1s of real blocking.
            warn_interval.tick().await;

            loop {
                tokio::select! {
                    biased;
                    result = &mut send_fut => {
                        match result {
                            Ok(()) => {
                                let elapsed = send_start.elapsed();
                                if elapsed >= std::time::Duration::from_millis(500) {
                                    let elapsed_ms = elapsed.as_millis() as u64;
                                    tracing::warn!(
                                        target = "pms_persist",
                                        block_id = %block_id,
                                        elapsed_ms,
                                        "persist_tx.send was back-pressured",
                                    );
                                    // v0.8.0: signal the resource guard
                                    // (pms-server side) so it arms read-only
                                    // mode proactively. Without this, the
                                    // 5 s queue-depth sampler misses sub-5s
                                    // saturations entirely — testnet
                                    // 2026-05-04 logged 6728 of these in
                                    // 24 h with 0 read-only auto-arms.
                                    crate::back_pressure::record_event(elapsed_ms);
                                }
                                break;
                            }
                            Err(_closed) => {
                                tracing::error!(
                                    target = "pms_persist",
                                    block_id = %block_id,
                                    "Persist channel closed — background task died. \
                                     Block NOT persisted. Returning error so caller can retry.",
                                );
                                return Err(anyhow::anyhow!(
                                    "persist pipeline down: background task is not running"
                                ));
                            }
                        }
                    }
                    _ = warn_interval.tick() => {
                        static STALL_COUNT: std::sync::atomic::AtomicU64 =
                            std::sync::atomic::AtomicU64::new(0);
                        let count = STALL_COUNT
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        // The interval ticks once per second after consuming
                        // the immediate first tick above, so each fire here
                        // means "we've been blocked for ~1 more second".
                        crate::metrics::PERSIST_STALL_SECONDS.inc();
                        tracing::error!(
                            target = "pms_persist",
                            block_id = %block_id,
                            elapsed_ms = send_start.elapsed().as_millis() as u64,
                            total_stalls = count,
                            "persist_tx.send still pending — RocksDB pipeline saturated. \
                             Caller is being back-pressured until the queue drains.",
                        );
                    }
                }
            }
        }

        let t_send = send_start.elapsed();
        let t_total = t0.elapsed();

        // Per-stage cumulative timing counters. Pair with `pms_persist_blocks_total`
        // to compute average µs/block per stage between two scrapes — that's
        // what `test_tps_degradation_profile` does to identify which stage
        // grows over time as the UTXO/DAG state expands.
        let stage_us = &crate::metrics::PERSIST_STAGE_US;
        stage_us
            .with_label_values(&["parents"])
            .inc_by(t_parents.as_micros() as u64);
        stage_us
            .with_label_values(&["utxo_val"])
            .inc_by(t_utxo_val.as_micros() as u64);
        stage_us
            .with_label_values(&["dag_val"])
            .inc_by(t_dag_val.as_micros() as u64);
        stage_us
            .with_label_values(&["utxo_ram"])
            .inc_by(t_utxo.as_micros() as u64);
        stage_us
            .with_label_values(&["dag_insert"])
            .inc_by(t_dag.as_micros() as u64);
        stage_us
            .with_label_values(&["send"])
            .inc_by(t_send.as_micros() as u64);
        crate::metrics::PERSIST_BLOCKS_TOTAL.inc();

        // Sample the same timing into the trace log every 500 blocks for
        // human-readable post-mortems. The Prometheus counters above are
        // the canonical source for dashboards and the profile test.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count.is_multiple_of(500) {
            tracing::info!(
                target = "pms_perf",
                block_id = %block_id,
                parents_us = t_parents.as_micros() as u64,
                utxo_val_us = t_utxo_val.as_micros() as u64,
                dag_val_us = t_dag_val.as_micros() as u64,
                utxo_ram_us = t_utxo.as_micros() as u64,
                dag_insert_us = t_dag.as_micros() as u64,
                send_us = t_send.as_micros() as u64,
                total_us = t_total.as_micros() as u64,
                "persist_block timing (µs)"
            );
        }

        // ============================================================
        // 8) Succes global - Client gets response BEFORE disk write
        // ============================================================

        // Incrementer le compteur de blocs pour ce mineur (node rewards)
        if !wb.signer_pk_hex.trim().is_empty() {
            if let Err(e) = self.store.increment_node_block_count(&wb.signer_pk_hex) {
                tracing::warn!("Failed to increment node block count: {}", e);
            }
        }

        // ============================================================
        // 9) Emettre BlockPersisted sur l'EventBus (SSE activity stream)
        // ============================================================
        {
            let (payload_type, involved) = match &payload {
                Some(PayloadEnvelope::Plain(plain)) => {
                    let ptype = plain_payload_type_str(plain);
                    let addrs = pms_wallet::history::collect_involved_addresses(plain);
                    (ptype, addrs)
                }
                Some(PayloadEnvelope::Encrypted(_)) => ("Encrypted", vec![]),
                None => ("Empty", vec![]),
            };
            let ts_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            self.event_bus.emit(PmsEvent::BlockPersisted {
                block_id,
                ts_ms,
                payload_type: payload_type.to_string(),
                involved_addresses: involved,
                payload_json: payload_json_for_event,
            });
        }

        Ok(PutResult::Inserted)
    }
}

/// Décision single-writer FAIL-CLOSED (audit H-4, v0.9.0).
///
/// Appelée quand `policy.enforce_single_writer == true` :
/// - **Active set non vide** : le signataire du bloc doit en faire partie.
/// - **Active set vide** : refus de TOUS les blocs en Testnet/Mainnet
///   (config corrompue ou clé bootstrap absente — on ne doit jamais ouvrir
///   l'écriture à n'importe quel auto-signataire) ; toléré en mode Dev pur
///   uniquement (aucune clé coordinator configurée, par design), avec warn.
///
/// Fonction pure pour rester unit-testable sans monter un CoreAdapter.
fn single_writer_gate(
    accepted_signer_keys: &std::collections::HashSet<String>,
    signer_pk_hex: &str,
    is_dev_mode: bool,
) -> Result<(), String> {
    if accepted_signer_keys.is_empty() {
        if is_dev_mode {
            tracing::warn!(
                "single_writer: empty active key set in Dev mode — enforcement skipped (fail-open by design in Dev only)"
            );
            return Ok(());
        }
        tracing::error!(
            "🚨 single_writer FAIL-CLOSED: enforce_single_writer=true but the active \
             coordinator key set is EMPTY (missing bootstrap key or corrupted rotation \
             state). Refusing all blocks until the configuration is fixed."
        );
        return Err(
            "no active coordinator key configured — all blocks refused (fail-closed)".to_string(),
        );
    }
    let signer = signer_pk_hex.trim().to_string();
    if !accepted_signer_keys.contains(&signer) {
        return Err(format!(
            "signer {signer} is not in the active coordinator key set ({} keys)",
            accepted_signer_keys.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod single_writer_tests {
    use super::single_writer_gate;
    use std::collections::HashSet;

    fn set(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn empty_active_set_rejects_everything_in_prod() {
        let r = single_writer_gate(&set(&[]), "04anykey", false);
        println!("empty set / prod: {r:?}");
        let err = r.expect_err("must fail closed");
        assert!(err.contains("fail-closed"), "got: {err}");
    }

    #[test]
    fn empty_active_set_tolerated_in_dev() {
        let r = single_writer_gate(&set(&[]), "04anykey", true);
        println!("empty set / dev: {r:?}");
        assert!(r.is_ok());
    }

    #[test]
    fn signer_in_active_set_accepted() {
        let r = single_writer_gate(&set(&["04coord"]), "04coord", false);
        println!("signer in set: {r:?}");
        assert!(r.is_ok());
    }

    #[test]
    fn signer_outside_active_set_rejected_even_in_dev() {
        // Dès qu'un active set existe, il s'applique aussi en Dev.
        let r = single_writer_gate(&set(&["04coord"]), "04attacker", true);
        println!("foreign signer / dev: {r:?}");
        assert!(r.is_err());
    }

    #[test]
    fn signer_whitespace_trimmed() {
        let r = single_writer_gate(&set(&["04coord"]), "  04coord  ", false);
        println!("trimmed signer: {r:?}");
        assert!(r.is_ok());
    }
}
