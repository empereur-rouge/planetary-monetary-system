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
use pms_storage::coordinator_key_store::KeyRotationRecord;
use pms_storage::{StoredBlock, UtxoDelta};
use pms_types::{Block, PayloadEnvelope, PlainPayload};
use pms_wire::WireBlock;

/// Single source for the BridgeMint replay reject reason, used by both the early
/// (durable + RAM) check and the authoritative atomic claim so the reject reason
/// can never drift by path (audit rang 3, B3).
fn bridge_replay_rejected(lock_block_id: &str) -> PutResult {
    PutResult::Rejected(format!(
        "bridge lock already consumed (replay): {lock_block_id}"
    ))
}

/// Reconcile a `BridgeMint`'s outputs against the source `BridgeLock` it claims
/// to back: the lock must be destined for `dest_ledger` (THIS ledger), and every
/// output must go to the lock's `dest_address` in the lock's `asset_id`, with the
/// outputs summing to EXACTLY the locked `amount`. Prevents minting more than was
/// locked (inflation), to the wrong recipient (theft), or onto a ledger other
/// than the lock's destination (cross-ledger double-mint — the `bridge_consumed`
/// anti-replay marker is per-destination-ledger, so without this check a single
/// lock could be minted once per ledger). (audit rang 3, B3)
fn reconcile_bridge_mint(
    outputs: &[pms_types::TxOutput],
    lock: &pms_interface::BridgeLockInfo,
    dest_ledger: &str,
) -> std::result::Result<(), String> {
    use rust_decimal::Decimal;
    use std::str::FromStr;
    if lock.dest_ledger_id != dest_ledger {
        return Err(format!(
            "bridge mint applied on ledger {dest_ledger} but lock is destined for {}",
            lock.dest_ledger_id
        ));
    }
    if outputs.is_empty() {
        return Err("bridge mint has no outputs".to_string());
    }
    let lock_amount = Decimal::from_str(&lock.amount)
        .map_err(|e| format!("bridge lock amount '{}' unparseable: {e}", lock.amount))?;
    let mut sum = Decimal::ZERO;
    for o in outputs {
        if o.asset_id != lock.asset_id {
            return Err(format!(
                "bridge mint asset {:?} != locked asset {:?}",
                o.asset_id, lock.asset_id
            ));
        }
        if o.address != lock.dest_address {
            return Err(format!(
                "bridge mint output address {} != locked dest_address {}",
                o.address, lock.dest_address
            ));
        }
        let amt = Decimal::from_str(&o.amount).map_err(|e| {
            format!("bridge mint output amount '{}' unparseable: {e}", o.amount)
        })?;
        if amt < Decimal::ZERO {
            return Err("bridge mint output amount is negative".to_string());
        }
        sum += amt;
    }
    if sum != lock_amount {
        return Err(format!(
            "bridge mint total {sum} != locked amount {lock_amount} (inflation guard)"
        ));
    }
    Ok(())
}

impl<S> CoreAdapter<S>
where
    S: pms_storage::EngineStorage,
{
    /// Résout les métadonnées « asset custom » d'un `asset_id` : un token
    /// (`token_registry`) OU une classe SFT (`sft_classes`, vue `TokenMetadata`
    /// via [`pms_types::SftClass::to_token_metadata`]). Les deux registres sont
    /// mutuellement exclusifs (namespace `:`), donc au plus un match. Fail-open à
    /// `None` sur erreur store. Point unique de résolution pour la validation de
    /// mint contraint ET la résolution du taux de demurrage.
    fn resolve_asset_metadata(&self, asset_id: &str) -> Option<pms_types::TokenMetadata> {
        // Fail-open view of the strict resolver: a store read error is swallowed
        // to `None` (used by the demurrage-rate lookup, where a transient miss is
        // tolerable). The royalty gate uses `resolve_asset_metadata_strict`.
        self.resolve_asset_metadata_strict(asset_id).unwrap_or(None)
    }

    /// **Fail-CLOSED** resolution for the settlement path (audit F5).
    ///
    /// Unlike [`Self::resolve_asset_metadata`] (which swallows a store read error
    /// to `None`), this distinguishes `Ok(None)` — genuinely no registry entry,
    /// royalty legitimately absent — from `Err` — a transient RocksDB failure.
    /// The MarketSettle royalty gate MUST NOT treat a read error as "no royalty":
    /// that would both silently drop the creator's cut AND fork consensus (nodes
    /// with divergent store health would resolve different royalties and
    /// accept/reject the same block differently). On `Err` the settlement is
    /// rejected so every node reaches the same verdict.
    fn resolve_asset_metadata_strict(
        &self,
        asset_id: &str,
    ) -> Result<Option<pms_types::TokenMetadata>, String> {
        match self.store.get_token(asset_id) {
            Ok(Some(m)) => return Ok(Some(m)),
            Ok(None) => {}
            Err(e) => return Err(format!("token registry read failed: {e}")),
        }
        match self.store.get_sft_class(asset_id) {
            Ok(Some(c)) => Ok(Some(c.to_token_metadata())),
            Ok(None) => Ok(None),
            Err(e) => Err(format!("sft registry read failed: {e}")),
        }
    }

    /// **Validation complète du plaintext d'un `TxUtxo`** — SOURCE UNIQUE de
    /// vérité partagée par le hot-path (payload `Plain`) ET les handlers qui
    /// chiffrent le payload (`wallet_send_tx`, `wallet_send_simple`).
    ///
    /// Exécute :
    ///   - [`validate_transaction_full`] : appariement input/unlock, signatures
    ///     ECDSA, binding ownership (C-1), autorisation des conditions de dépense
    ///     (MultiSig quorum / HashLock préimage), time-locks des inputs, **dédup
    ///     des inputs dupliqués** (anti-inflation), conservation par-asset
    ///     (demurrage-aware) ;
    ///   - **gel compliance** sur chaque adresse propriétaire d'input ET chaque
    ///     adresse de sortie.
    ///
    /// Retourne les outputs des inputs résolus (utile aux appelants pour dériver
    /// l'émetteur / le change).
    ///
    /// # Sécurité
    /// Un payload `TxUtxo` **chiffré** est opaque pour `persist_block` : le
    /// hot-path saute `validate_transaction_full`. Les handlers qui chiffrent un
    /// `TxUtxo` DOIVENT donc appeler cette fonction sur le plaintext AVANT
    /// chiffrement, sinon TOUS ces contrôles sont contournés (audit 2026-06,
    /// cause A : bypass compliance/time-lock/MultiSig + inflation par input
    /// dupliqué). Les deux chemins appelant cette MÊME fonction ne peuvent pas
    /// diverger.
    pub(crate) async fn validate_plain_txutxo(
        &self,
        tx: &pms_types::Transaction,
        policy: &crate::ValidatePolicy,
        now_ms: u64,
    ) -> Result<Vec<pms_types::TxOutput>, String> {
        use crate::validations::transactions::validate_transaction_full;

        // Résout les taux de demurrage des assets custom touchés (cf. hot-path).
        let assets: std::collections::HashSet<&str> = tx
            .outputs
            .iter()
            .filter_map(|o| o.asset_id.as_deref())
            .collect();
        let demurrage_rates: std::collections::HashMap<String, u32> = assets
            .into_iter()
            .filter_map(|asset| {
                self.resolve_asset_metadata(asset)
                    .and_then(|m| m.demurrage_bps_per_day.filter(|bps| *bps > 0))
                    .map(|bps| (asset.to_string(), bps))
            })
            .collect();

        let tx_input_outputs =
            validate_transaction_full(&self.utxos, tx, policy, now_ms, &demurrage_rates)
                .await
                .map_err(|e| format!("utxo validation failed: {e}"))?;

        // Compliance : ni un input gelé, ni une sortie vers une adresse gelée.
        for out in &tx_input_outputs {
            if self.store.is_frozen(&out.address).unwrap_or(false) {
                return Err(format!(
                    "compliance: sender address is frozen: {}",
                    out.address
                ));
            }
        }
        for out in &tx.outputs {
            if self.store.is_frozen(&out.address).unwrap_or(false) {
                return Err(format!(
                    "compliance: recipient address is frozen: {}",
                    out.address
                ));
            }
        }

        Ok(tx_input_outputs)
    }

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

            // Verification 3: MINT CONTRAINT PER-ASSET (plan 2.3 / 2.4, v0.10.0)
            //
            // Pour les outputs d'assets custom, le protocole enforce désormais
            // TokenMetadata : asset enregistré, signer == mint_authority,
            // granularité decimals, et supply cap (circulating + mint <=
            // max_supply, supply cache du ShardedUtxoSet). Le gate Coordinator
            // ci-dessus reste appliqué — ce check est per-asset, en plus.
            {
                use crate::validations::mint::{
                    minted_amounts_by_custom_asset, validate_custom_asset_mints,
                };
                let minted = match minted_amounts_by_custom_asset(outputs) {
                    Ok(m) => m,
                    Err(e) => {
                        return Ok(PutResult::Rejected(format!("mint amounts: {e}")));
                    }
                };
                if !minted.is_empty() {
                    let now_ms_mint = now_ms_for_signers.max(0) as u64;
                    let mut metadata = std::collections::HashMap::new();
                    let mut circulating = std::collections::HashMap::new();
                    let mut locked_collateral = std::collections::HashMap::new();
                    for asset_id in minted.keys() {
                        // NOTE fail-open assumé : une erreur RocksDB sur le
                        // lookup registry est traitée comme « non enregistré »
                        // (contraintes per-asset skippées, gate Coordinator
                        // conservé). À durcir avec la migration ApiError.
                        //
                        // Token OU classe SFT (mutuellement exclusifs, namespace `:`)
                        // → MÊME validation de mint contraint (mint_authority +
                        // max_supply). Résolution centralisée.
                        let meta = self.resolve_asset_metadata(asset_id);
                        // Le supply cache n'est interrogé que si une cap OU un
                        // collatéral existe — validate_custom_asset_mints
                        // traite une entrée absente comme ZERO.
                        if meta
                            .as_ref()
                            .is_some_and(|m| m.max_supply.is_some() || m.collateral_address.is_some())
                        {
                            let (supply, _count) = self
                                .utxos
                                .circulating_supply_by_asset(Some(asset_id))
                                .await;
                            circulating.insert(asset_id.clone(), supply);
                        }
                        // Mint collatéralisé (2.3 v2) : somme des UTXOs de
                        // réserve ENCORE time-lockés à l'adresse déclarée
                        // (même ledger). Adresse de réserve dédiée → peu
                        // d'UTXOs, lookup par-mint (pas le hot path tx).
                        if let Some(m) = meta.as_ref() {
                            if let Some(reserve_addr) = &m.collateral_address {
                                let reserve_utxos =
                                    self.utxos.utxos_by_address(reserve_addr).await;
                                let locked = crate::validations::mint::sum_locked_collateral(
                                    &reserve_utxos,
                                    &m.collateral_asset_id,
                                    now_ms_mint,
                                );
                                locked_collateral.insert(asset_id.clone(), locked);
                            }
                        }
                        metadata.insert(asset_id.clone(), meta);
                    }
                    if let Err(e) = validate_custom_asset_mints(
                        outputs,
                        &wb.signer_pk_hex,
                        &minted,
                        &metadata,
                        &circulating,
                        &locked_collateral,
                    ) {
                        tracing::warn!(
                            "🚫 Custom-asset mint blocked on block {}: {e}",
                            &wb.id[..16.min(wb.id.len())]
                        );
                        return Ok(PutResult::Rejected(format!("token mint: {e}")));
                    }
                }
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

        // 1.gov) Gouvernance timelock (plan §4). Proposal → stocke le record
        // (Pending) ; Enact → vérifie timelock écoulé + statut Pending puis
        // applique le ConfigUpdate (réutilise apply_config_update) ; Cancel →
        // marque Cancelled. Les checks statut + timelock rendent l'enact
        // idempotent ET inviolable (rejet avant expiration → aucun bloc forgé).
        if let Some(PayloadEnvelope::Plain(PlainPayload::GovernanceProposal {
            proposal_id,
            update,
            tier,
            reason,
            announced_at_ms,
            enact_after_ms,
        })) = &payload
        {
            // Le proposal_id est content-addressed (SHA-256 du changement) — mais
            // `put_governance_proposal` est un write aveugle. On garde l'unicité
            // ICI, au niveau DAG (source de vérité) : un id déjà connu ne doit
            // JAMAIS écraser silencieusement un record existant (overwrite d'une
            // proposition Pending/Enacted/Cancelled). Une vraie re-proposition
            // obtient un id distinct via un `announced_at_ms` frais.
            match self.store.get_governance_proposal(proposal_id) {
                Ok(Some(_)) => {
                    return Ok(PutResult::Rejected(format!(
                        "governance proposal already exists: {proposal_id}"
                    )));
                }
                Ok(None) => {}
                Err(e) => {
                    return Ok(PutResult::Rejected(format!(
                        "governance proposal lookup failed: {e}"
                    )));
                }
            }
            // PAS de check `announced_at ≈ horloge-du-validateur` ICI : la
            // validation persist est ré-exécutée lors du SYNC P2P (blocks.rs) et du
            // replay — comparer `announced_at` à l'horloge COURANTE rejetterait tout
            // bloc de gouvernance historique resynchronisé (l'horloge a avancé) →
            // sync non-déterministe. Dans le modèle single-writer, seul le
            // Coordinator (de confiance) peut forger une proposition, et son handler
            // `admin_propose` estampille `announced_at = now`. La garantie « pas de
            // back-dating » repose sur la TRANSPARENCE : le bloc proposal apparaît
            // dans le DAG en temps réel, observable par le public, qui compare
            // `announced_at` à l'arrivée réelle du bloc. La validation ci-dessous
            // (relation `enact_after == announced_at + durée`) est déterministe et
            // sûre au replay/sync.
            //
            // PALIER MINIMUM (G4) — le palier déclaré doit respecter le minimum du
            // paramètre (table §2). Empêche de faire passer un changement Policy/
            // Constitution en « Operator 7 j ».
            if let Err(reason) = pms_config::validate_tier(update, *tier) {
                return Ok(PutResult::Rejected(reason));
            }
            // ASYMÉTRIE tighten/loosen (G5) — la DIRECTION est calculée par le
            // protocole depuis la config COURANTE, pas déclarée par le proposant.
            // On re-dérive l'`enact_after` attendu (instantané si tighten, plein
            // sinon) et on rejette toute incohérence : un proposant ne peut donc
            // pas réclamer un timelock court pour un desserrage.
            let current_cfg = match self.store.get_runtime_config() {
                Ok(c) => c,
                Err(e) => {
                    return Ok(PutResult::Rejected(format!(
                        "governance proposal: runtime config unavailable: {e}"
                    )));
                }
            };
            let expected_timelock = pms_config::required_timelock_ms(update, &current_cfg, *tier);
            let expected_enact_after = announced_at_ms.saturating_add(expected_timelock);
            if *enact_after_ms != expected_enact_after {
                return Ok(PutResult::Rejected(format!(
                    "governance proposal: enact_after mismatch (declared={}, expected announced_at+{}ms={})",
                    enact_after_ms, expected_timelock, expected_enact_after
                )));
            }
            let record = pms_config::GovernanceProposalRecord {
                proposal_id: proposal_id.clone(),
                update: update.clone(),
                tier: *tier,
                reason: reason.clone(),
                announced_at_ms: *announced_at_ms,
                enact_after_ms: *enact_after_ms,
                status: pms_config::GovernanceStatus::Pending,
                proposal_block_id: wb.id.clone(),
                enact_block_id: None,
                cancel_block_id: None,
            };
            if let Err(e) = self.store.put_governance_proposal(&record) {
                return Ok(PutResult::Rejected(format!(
                    "governance proposal store failed: {e}"
                )));
            }
            tracing::info!(
                "🏛️ Governance proposal {} ({}) announced — enact_after={} (block {})",
                proposal_id,
                tier.as_str(),
                enact_after_ms,
                wb.id
            );
        }
        if let Some(PayloadEnvelope::Plain(PlainPayload::GovernanceEnact {
            proposal_id, ..
        })) = &payload
        {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let record = match self.store.get_governance_proposal(proposal_id) {
                Ok(Some(r)) => r,
                Ok(None) => {
                    return Ok(PutResult::Rejected(format!(
                        "governance enact: unknown proposal {proposal_id}"
                    )));
                }
                Err(e) => {
                    return Ok(PutResult::Rejected(format!(
                        "governance enact lookup failed: {e}"
                    )));
                }
            };
            if record.status != pms_config::GovernanceStatus::Pending {
                return Ok(PutResult::Rejected(format!(
                    "governance enact: proposal {proposal_id} not pending (status={})",
                    record.status.as_str()
                )));
            }
            // INVARIANT TIMELOCK (G2) — refus tant que le délai n'est pas écoulé.
            if now_ms < record.enact_after_ms {
                return Ok(PutResult::Rejected(format!(
                    "governance enact: timelock not elapsed (now={now_ms}, enact_after={})",
                    record.enact_after_ms
                )));
            }
            if let Err(e) = self
                .store
                .apply_config_update(&record.update, &wb.id, now_ms as i64)
            {
                return Ok(PutResult::Rejected(format!(
                    "governance enact apply failed: {e}"
                )));
            }
            if let Err(e) = self.store.set_governance_status(
                proposal_id,
                pms_config::GovernanceStatus::Enacted,
                &wb.id,
            ) {
                tracing::warn!("governance status update failed: {e}");
            }
            tracing::info!(
                "🏛️ Governance proposal {} ENACTED (block {})",
                proposal_id,
                wb.id
            );
        }
        if let Some(PayloadEnvelope::Plain(PlainPayload::GovernanceCancel {
            proposal_id, ..
        })) = &payload
        {
            let record = match self.store.get_governance_proposal(proposal_id) {
                Ok(Some(r)) => r,
                Ok(None) => {
                    return Ok(PutResult::Rejected(format!(
                        "governance cancel: unknown proposal {proposal_id}"
                    )));
                }
                Err(e) => {
                    return Ok(PutResult::Rejected(format!(
                        "governance cancel lookup failed: {e}"
                    )));
                }
            };
            if record.status != pms_config::GovernanceStatus::Pending {
                return Ok(PutResult::Rejected(format!(
                    "governance cancel: proposal {proposal_id} not pending (status={})",
                    record.status.as_str()
                )));
            }
            if let Err(e) = self.store.set_governance_status(
                proposal_id,
                pms_config::GovernanceStatus::Cancelled,
                &wb.id,
            ) {
                return Ok(PutResult::Rejected(format!("governance cancel failed: {e}")));
            }
            tracing::info!(
                "🏛️ Governance proposal {} CANCELLED (block {})",
                proposal_id,
                wb.id
            );
        }

        // 1.sft) Enregistrement d'une classe semi-fongible (SFT, spec semi-fungibles).
        // Le registre est la source de vérité des métadonnées + contraintes de mint ;
        // les soldes vivent dans le moteur UTXO (rien d'autre à appliquer ici).
        if let Some(PayloadEnvelope::Plain(PlainPayload::SftClassCreate(class))) = &payload {
            // Segment valide = [a-z0-9-], 1..=32 (collection / classe).
            let valid_seg = |s: &str| {
                !s.is_empty()
                    && s.len() <= 32
                    && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            };
            if !valid_seg(&class.collection_id) || !valid_seg(&class.class_id) {
                return Ok(PutResult::Rejected(
                    "sft class: collection_id/class_id must be [a-z0-9-]{1,32}".to_string(),
                ));
            }
            // L'asset_id DOIT être exactement "collection:class" (cohérence + namespace
            // collision-free : un token ne peut pas contenir ':').
            let expected_asset_id = format!("{}:{}", class.collection_id, class.class_id);
            if class.asset_id != expected_asset_id {
                return Ok(PutResult::Rejected(format!(
                    "sft class: asset_id must equal \"{expected_asset_id}\" (got \"{}\")",
                    class.asset_id
                )));
            }
            if class.name.trim().is_empty() || class.name.len() > 128 {
                return Ok(PutResult::Rejected(
                    "sft class: name required (1..=128 chars)".to_string(),
                ));
            }
            if class.decimals > 18 {
                return Ok(PutResult::Rejected("sft class: decimals must be <= 18".to_string()));
            }
            if let Some(ms) = &class.max_supply {
                // Doit être un décimal STRICTEMENT positif (parité avec
                // `validate_token_metadata` : un cap nul/négatif n'a pas de sens).
                match ms.parse::<rust_decimal::Decimal>() {
                    Ok(d) if d > rust_decimal::Decimal::ZERO => {}
                    _ => {
                        return Ok(PutResult::Rejected(
                            "sft class: max_supply must be a positive decimal".to_string(),
                        ));
                    }
                }
            }
            if class.creator.trim().is_empty() || class.mint_authority.trim().is_empty() {
                return Ok(PutResult::Rejected(
                    "sft class: creator and mint_authority required".to_string(),
                ));
            }
            if let Some(bps) = class.demurrage_bps_per_day {
                if bps > 10_000 {
                    return Ok(PutResult::Rejected(
                        "sft class: demurrage_bps_per_day must be <= 10000".to_string(),
                    ));
                }
            }
            // royalty de revente (2.7) : cap 10_000 bps + bénéficiaire non-vide.
            // Source unique partagée avec le registre token (register_token).
            if let Err(e) = pms_types::validate_royalty_fields(
                class.royalty_bps,
                class.royalty_beneficiary.as_deref(),
            ) {
                return Ok(PutResult::Rejected(format!("sft class: {e}")));
            }
            // Unicité : une classe déjà enregistrée ne doit pas être écrasée
            // silencieusement (anti-overwrite, comme la gouvernance).
            match self.store.get_sft_class(&class.asset_id) {
                Ok(Some(_)) => {
                    return Ok(PutResult::Rejected(format!(
                        "sft class already exists: {}",
                        class.asset_id
                    )));
                }
                Ok(None) => {}
                Err(e) => {
                    return Ok(PutResult::Rejected(format!("sft class lookup failed: {e}")));
                }
            }
            if let Err(e) = self.store.put_sft_class(class) {
                return Ok(PutResult::Rejected(format!("sft class store failed: {e}")));
            }
            tracing::info!(
                "🎟️ SFT class registered: {} (\"{}\", block {})",
                class.asset_id,
                class.name,
                wb.id
            );
        }

        // 1.royalty) Mise à jour de la politique royalty d'un asset existant
        // (protocole 2.7). AUTORISÉE PAR LA CO-SIGNATURE DU BÉNÉFICIAIRE COURANT
        // (pas le coordinateur) : le Coordinator forge le bloc mais ne peut PAS
        // rediriger la royalty sans la signature de l'ayant droit actuel.
        if let Some(PayloadEnvelope::Plain(PlainPayload::RoyaltyUpdate {
            asset_id,
            royalty_bps,
            royalty_beneficiary,
            auth_pubkey_hex,
            auth_signature_b64,
        })) = &payload
        {
            // (a) Nouveaux champs validés (cap + bénéficiaire non-vide).
            if let Err(e) =
                pms_types::validate_royalty_fields(*royalty_bps, royalty_beneficiary.as_deref())
            {
                return Ok(PutResult::Rejected(format!("royalty update: {e}")));
            }
            // (b) Politique COURANTE (fail-closed) → autorisateur légitime =
            //     bénéficiaire explicite courant, à défaut le `creator`.
            let current_meta = match self.resolve_asset_metadata_strict(asset_id) {
                Ok(Some(m)) => m,
                Ok(None) => {
                    return Ok(PutResult::Rejected(format!(
                        "royalty update: asset not found: {asset_id}"
                    )));
                }
                Err(e) => return Ok(PutResult::Rejected(format!("royalty update lookup: {e}"))),
            };
            let current_authorizer = current_meta
                .royalty_beneficiary
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| current_meta.creator.as_str());
            // (c) La signature DOIT porter sur EXACTEMENT ce changement (asset +
            //     nouvelle politique + network_id), ET provenir de l'autorisateur
            //     courant. Anti-forge + anti-replay (un ancien bénéficiaire n'est
            //     plus l'autorisateur courant → sa signature rejouée échoue).
            // La signature commit à la VERSION COURANTE (anti-replay : une sig
            // capturée sur-DAG devient invalide dès qu'un changement fait avancer
            // `royalty_version`).
            let consumed_version = current_meta.royalty_version;
            let next_version = consumed_version.saturating_add(1);
            let msg = pms_types::royalty_update_signing_message(
                &self.wire_meta.network_id,
                asset_id,
                *royalty_bps,
                royalty_beneficiary.as_deref(),
                consumed_version,
            );
            if crate::validations::signature::verify_detached_signature(
                msg.as_bytes(),
                auth_pubkey_hex,
                auth_signature_b64,
            )
            .is_err()
            {
                return Ok(PutResult::Rejected(
                    "royalty update: invalid authorization signature".to_string(),
                ));
            }
            if !crate::validations::ownership::unlock_matches_address(
                auth_pubkey_hex,
                current_authorizer,
            ) {
                return Ok(PutResult::Rejected(
                    "royalty update: signer is not the current royalty beneficiary".to_string(),
                ));
            }
            // (d) Écriture sur le registre correspondant (token OU classe SFT,
            //     namespace `:` exclusif). Refus si introuvable (race improbable).
            match self.store.get_sft_class(asset_id) {
                Ok(Some(mut class)) => {
                    class.royalty_bps = *royalty_bps;
                    class.royalty_beneficiary = royalty_beneficiary.clone();
                    class.royalty_version = next_version; // anti-replay : monotone
                    if let Err(e) = self.store.put_sft_class(&class) {
                        return Ok(PutResult::Rejected(format!("royalty update store: {e}")));
                    }
                    tracing::info!(
                        "👑 Royalty updated (SFT class {}): {:?} bps → {:?} v{} (block {})",
                        asset_id, royalty_bps, royalty_beneficiary, next_version, wb.id
                    );
                }
                Ok(None) => match self.store.get_token(asset_id) {
                    Ok(Some(mut meta)) => {
                        meta.royalty_bps = *royalty_bps;
                        meta.royalty_beneficiary = royalty_beneficiary.clone();
                        meta.royalty_version = next_version; // anti-replay : monotone
                        if let Err(e) = self.store.put_token(&meta) {
                            return Ok(PutResult::Rejected(format!("royalty update store: {e}")));
                        }
                        tracing::info!(
                            "👑 Royalty updated (token {}): {:?} bps → {:?} v{} (block {})",
                            asset_id, royalty_bps, royalty_beneficiary, next_version, wb.id
                        );
                    }
                    Ok(None) => {
                        return Ok(PutResult::Rejected(format!(
                            "royalty update: asset not found: {asset_id}"
                        )));
                    }
                    Err(e) => {
                        return Ok(PutResult::Rejected(format!("royalty update lookup: {e}")));
                    }
                },
                Err(e) => {
                    return Ok(PutResult::Rejected(format!("royalty update lookup: {e}")));
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
        // Horloge UNIQUE du bloc : la même valeur sert à la validation
        // temporelle (time-lock, demurrage) ET à l'estampillage `created_at`
        // des UTXOs créés — toute divergence fausserait le calcul de décote.
        let now_ms = now_ms_for_signers.max(0) as u64;
        if let Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx))) = &block.payload {
            // SOURCE UNIQUE de validation TxUtxo (validate_transaction_full +
            // gel compliance inputs/outputs), partagée avec les handlers de
            // payload CHIFFRÉ via `validate_txutxo_full` — pour qu'aucun contrôle
            // ne puisse exister sur un chemin et manquer sur l'autre (audit
            // 2026-06, cause A). Mêmes messages de rejet qu'avant.
            if let Err(e) = self.validate_plain_txutxo(tx, policy, now_ms).await {
                return Ok(PutResult::Rejected(e));
            }
        }
        // ─── MarketSettle (protocole 2.7) : règlement atomique + royalty enforced ───
        // Réutilise la MÊME validation UTXO que TxUtxo (signatures, ownership,
        // conservation par-asset, compliance) via `validate_plain_txutxo`, PUIS
        // impose le gate royalty/rôles (`validations::market`). La royalty est
        // RÉ-DÉRIVÉE du registre de `asset_sold` — impossible pour le builder de
        // sous-payer le créateur : un settlement non conforme est REJETÉ ici.
        if let Some(PayloadEnvelope::Plain(PlainPayload::MarketSettle {
            tx,
            asset_sold,
            quantity,
            price_asset,
            price,
            seller,
            buyer,
        })) = &block.payload
        {
            use crate::validations::market;
            // 1. Validation UTXO complète (source unique partagée avec TxUtxo).
            //    Retourne les UTXOs dépensés résolus (adresse propriétaire + asset)
            //    nécessaires au binding de rôle vendeur/acheteur.
            let input_outputs = match self.validate_plain_txutxo(tx, policy, now_ms).await {
                Ok(outs) => outs,
                Err(e) => return Ok(PutResult::Rejected(format!("settlement tx: {e}"))),
            };
            // 2. Montants déclarés.
            let Ok(qty) = rust_decimal::Decimal::from_str_exact(quantity) else {
                return Ok(PutResult::Rejected(
                    "settlement: quantity not a decimal".to_string(),
                ));
            };
            let Ok(price_dec) = rust_decimal::Decimal::from_str_exact(price) else {
                return Ok(PutResult::Rejected("settlement: price not a decimal".to_string()));
            };
            // 3. Politique royalty de l'ASSET VENDU (registre) + décimales du prix.
            //    Résolution FAIL-CLOSED (audit F5) : une erreur store rejette (pas
            //    de bypass royalty ni de fork consensus). Ok(None) = pas de
            //    politique → royalty 0 (swap atomique pur, toujours valide).
            let sold_meta = match self.resolve_asset_metadata_strict(asset_sold) {
                Ok(m) => m,
                Err(e) => return Ok(PutResult::Rejected(format!("settlement: {e}"))),
            };
            let (royalty, beneficiary) = match sold_meta.and_then(|m| m.effective_royalty()) {
                Some((bps, b)) => {
                    let price_meta = match price_asset.as_deref() {
                        Some(a) => match self.resolve_asset_metadata_strict(a) {
                            Ok(m) => m,
                            Err(e) => return Ok(PutResult::Rejected(format!("settlement: {e}"))),
                        },
                        None => None,
                    };
                    let dec = market::price_decimals(price_meta.map(|m| m.decimals));
                    // compute_royalty is fallible (overflow → None, audit F3).
                    match market::compute_royalty(price_dec, bps, dec) {
                        Some(r) => (r, Some(b)),
                        None => {
                            return Ok(PutResult::Rejected(
                                "settlement: royalty computation overflow".to_string(),
                            ));
                        }
                    }
                }
                None => (rust_decimal::Decimal::ZERO, None),
            };
            // 4. Gate forme + binding de rôle.
            let check = market::SettlementCheck {
                asset_sold,
                quantity: qty,
                price_asset: price_asset.as_deref(),
                price: price_dec,
                seller,
                buyer,
                royalty,
                beneficiary: beneficiary.as_deref(),
            };
            if let Err(e) = market::validate_settlement(&input_outputs, &tx.outputs, &check) {
                // `e` is already self-prefixed with "settlement:" — don't double it.
                tracing::warn!(
                    "🚫 MarketSettle rejected on block {}: {e}",
                    &wb.id[..16.min(wb.id.len())]
                );
                return Ok(PutResult::Rejected(e));
            }
            tracing::info!(
                "🛒 MarketSettle OK: buyer={} qty={} of {} price={} {} (royalty {} → {}) block={}",
                &buyer[..20.min(buyer.len())],
                quantity,
                asset_sold,
                price,
                price_asset.as_deref().unwrap_or("PMS"),
                royalty,
                beneficiary.as_deref().unwrap_or("-"),
                &wb.id[..16.min(wb.id.len())],
            );
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

        // BridgeMint anti-replay (audit rang 3, B3) — EARLY reject (optimization).
        // A BridgeMint creates funds backed by a source-ledger BridgeLock; each
        // lock may be minted AT MOST ONCE. A replayed/re-signed BridgeMint reusing
        // an already-minted `lock_block_id` would re-mint out of nothing
        // (inflation). This skips the expensive UTXO validation below for a replay
        // we can already see: the `bridge_consumed` CF (authoritative across
        // restarts) OR the in-flight RAM claim. The AUTHORITATIVE guard is the
        // atomic `try_consume_bridge_lock` at the commit point (just before
        // `apply_diff`, mirroring the double-spend guard) — this is purely an
        // early-out.
        if let Some(PayloadEnvelope::Plain(PlainPayload::BridgeMint { lock_block_id, .. })) =
            &block.payload
        {
            let durably_consumed = match self.store.is_bridge_lock_consumed(lock_block_id).await {
                Ok(c) => c,
                Err(e) => {
                    return Ok(PutResult::Rejected(format!(
                        "bridge_consumed lookup failed: {e}"
                    )));
                }
            };
            if durably_consumed || self.dag.is_bridge_lock_consumed_ram(lock_block_id) {
                return Ok(bridge_replay_rejected(lock_block_id));
            }
        }

        // BridgeMint cross-ledger reconciliation (audit rang 3, B3). The mint must
        // EXACTLY back a real source `BridgeLock`: same total amount, same asset,
        // same recipient. Without this a coordinator BUG (or key compromise) could
        // mint MORE than was locked (inflation) or to the WRONG recipient (theft).
        // This (destination) adapter's store is prefix-scoped and can't read the
        // source ledger, so it delegates to the injected cross-ledger
        // `BridgeLockResolver` (the LedgerManager). FAIL-CLOSED: a BridgeMint with
        // no resolver wired, or referencing a non-existent lock, is rejected.
        if let Some(PayloadEnvelope::Plain(PlainPayload::BridgeMint {
            outputs,
            lock_block_id,
            source_ledger_id,
        })) = &block.payload
        {
            // Clone the (resolver, ledger_id) pair and drop the lock guard BEFORE
            // awaiting the resolver (parking_lot guards are not Send and must not
            // be held across .await).
            let wired = self.bridge_resolver.read().clone();
            let Some((resolver, my_ledger_id)) = wired else {
                return Ok(PutResult::Rejected(
                    "bridge mint reconciliation unavailable: no resolver wired".to_string(),
                ));
            };
            let lock = match resolver
                .resolve_bridge_lock(source_ledger_id, lock_block_id)
                .await
            {
                Ok(Some(l)) => l,
                Ok(None) => {
                    return Ok(PutResult::Rejected(format!(
                        "bridge mint references unknown lock {lock_block_id} on source ledger {source_ledger_id}"
                    )));
                }
                Err(e) => {
                    return Ok(PutResult::Rejected(format!(
                        "bridge lock resolution failed: {e}"
                    )));
                }
            };
            if let Err(e) = reconcile_bridge_mint(outputs, &lock, &my_ledger_id) {
                return Ok(PutResult::Rejected(e));
            }
        }

        // TokenBurn (plan §3.1, voie B) : validation complète owner-signée +
        // conservation-burn (inputs = change + amount détruit). Hot path, comme
        // TxUtxo/BridgeLock.
        if let Some(PayloadEnvelope::Plain(PlainPayload::TokenBurn {
            tx,
            asset_id,
            amount,
            owner,
        })) = &block.payload
        {
            use crate::validations::transactions::validate_token_burn_async;
            let burn_input_outputs = match validate_token_burn_async(
                &self.utxos,
                tx,
                asset_id,
                amount,
                owner,
                policy,
                now_ms,
            )
            .await
            {
                Ok(outs) => outs,
                Err(e) => {
                    return Ok(PutResult::Rejected(format!(
                        "token burn validation failed: {e}"
                    )));
                }
            };
            // Compliance : le burner (et donc le change, qui lui revient) ne
            // doit pas être gelé.
            for out in &burn_input_outputs {
                if self.store.is_frozen(&out.address).unwrap_or(false) {
                    return Ok(PutResult::Rejected(format!(
                        "compliance: burner address is frozen: {}",
                        out.address
                    )));
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
        // Demurrage 2.5 : chaque UTXO créé est estampillé `created_at` par le
        // SYSTÈME (même horloge `now_ms` que la validation) — toute valeur
        // client est écrasée (anti-antidatage). Base du calcul de décote.
        let stamp = |out: &pms_types::TxOutput| -> pms_types::TxOutput {
            pms_types::TxOutput {
                created_at: Some(now_ms),
                ..out.clone()
            }
        };
        // Forme canonique des `create` du delta : un seul endroit construit
        // les tuples (block_id, index, output estampillé) — un futur payload
        // à UTXO ne peut pas oublier le stamp.
        let stamped_creates = |outs: &[pms_types::TxOutput]| -> Vec<(String, u32, pms_types::TxOutput)> {
            outs.iter()
                .enumerate()
                .map(|(i, out)| (sb.id.clone(), i as u32, stamp(out)))
                .collect()
        };

        let delta = if let Some(d) = external_delta {
            Some(d)
        } else {
            match &payload {
            Some(PayloadEnvelope::Plain(PlainPayload::Mint { outputs })) => {
                // Mint = create only (no inputs)
                Some(UtxoDelta {
                    spend: vec![],
                    create: stamped_creates(outputs),
                })
            }

            // MarketSettle partage EXACTEMENT la mécanique UTXO de TxUtxo (dépense
            // les inputs, crée les outputs item/royalty/net/change, brûle le gas
            // `tx.fee` → part treasury). Même arme = zéro divergence de delta.
            Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx)))
            | Some(PayloadEnvelope::Plain(PlainPayload::MarketSettle { tx, .. })) => {
                // Tx = spend inputs + create outputs
                let spend = tx
                    .inputs
                    .iter()
                    .map(|inp| (inp.out.txid.clone(), inp.out.index))
                    .collect();

                let create = stamped_creates(&tx.outputs);

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

            Some(PayloadEnvelope::Plain(PlainPayload::TokenBurn { tx, .. })) => {
                // TokenBurn (plan §3.1, voie B) = spend inputs + create CHANGE
                // outputs. La part brûlée (inputs − change) ne crée aucun output
                // → la supply baisse. Pas d'accumulation de fee (un burn ne paie
                // pas de frais — garanti par validate_token_burn_async).
                let spend = tx
                    .inputs
                    .iter()
                    .map(|inp| (inp.out.txid.clone(), inp.out.index))
                    .collect();
                let create = stamped_creates(&tx.outputs);
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
                // fee_outputs puis reward_outputs — l'ordre fixe l'index des
                // OutputId (cf. PlainPayload::outputs()). Toujours PMS natif :
                // asset_id forcé à None.
                let create: Vec<(String, u32, pms_types::TxOutput)> = fee_outputs
                    .iter()
                    .chain(reward_outputs.iter())
                    .enumerate()
                    .map(|(i, out)| {
                        (
                            sb.id.clone(),
                            i as u32,
                            pms_types::TxOutput {
                                asset_id: None,
                                ..stamp(out)
                            },
                        )
                    })
                    .collect();

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
                Some(UtxoDelta {
                    spend: vec![],
                    create: stamped_creates(outputs),
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
                Some(UtxoDelta {
                    spend,
                    create: stamped_creates(outputs),
                })
            }
            Some(PayloadEnvelope::Plain(PlainPayload::Reverse {
                inputs, outputs, ..
            })) => {
                let spend = inputs
                    .iter()
                    .map(|inp| (inp.out.txid.clone(), inp.out.index))
                    .collect();
                Some(UtxoDelta {
                    spend,
                    create: stamped_creates(outputs),
                })
            }
                // Freeze/Unfreeze: no UTXO changes (registry-only)
                _ => None,
            }
        };

        // Note: append_block_atomic_with_utxo prend Option<&UtxoDelta>
        // === ASYNC PERSISTENCE: Update RAM first, persist in background ===

        // IDEMPOTENCE (audit S4) — déduplication AVANT toute mutation d'état.
        // Un bloc déjà présent dans le DAG (re-gossip réseau, retry client,
        // replay malveillant) ne DOIT PAS ré-appliquer son `UtxoDelta` : sinon
        // la supply double / les UTXOs sont re-crédités à chaque re-soumission
        // (inflation, classe du bug double-apply v0.7.20). Ce check DOIT
        // précéder `apply_diff` ci-dessous. Le placer après (ancien ordre)
        // appliquait le delta en RAM PUIS renvoyait AlreadyExists — masquant la
        // double-application et faisant diverger RAM/disk (le persist disque,
        // lui, est gated par ce même early-return, donc jamais ré-écrit).
        if self.dag.contains_block(&sb.id) {
            return Ok(PutResult::AlreadyExists);
        }

        // ── Bridge-mint anti-replay CLAIM (atomic, in-process commit point) ──
        // The durable early reject above (validation section) catches replays of
        // already-persisted locks; THIS atomic claim is the authoritative commit
        // point that closes the validate→apply window for two concurrent mints
        // of the SAME source lock racing before either's durable write lands.
        // `try_consume_bridge_lock` is an atomic DashSet test-and-set: exactly
        // one wins, the rest are rejected here (mirrors the double-spend guard).
        // (audit rang 3, B3)
        if let Some(PayloadEnvelope::Plain(PlainPayload::BridgeMint { lock_block_id, .. })) =
            &payload
        {
            if !self.dag.try_consume_bridge_lock(lock_block_id) {
                return Ok(bridge_replay_rejected(lock_block_id));
            }
        }

        let t0 = std::time::Instant::now();

        // 5.a) UTXO RAM Update FIRST (essential for preventing double-spend)
        //
        // Uses apply_diff() which groups operations by shard for minimal lock
        // acquisitions instead of sequential per-UTXO awaits.
        if let Some(d) = &delta {
            // ── Double-spend guard (atomic, lock-free) ──────────────────────
            // Claim every spent outpoint in the authoritative spent-set BEFORE
            // applying the UTXO delta. `try_mark_spent` is an atomic DashSet
            // test-and-set: when two concurrent blocks race on the same input,
            // the first claims it, the rest get `false` and are rejected here.
            // This is the commit point that closes the validate→apply TOCTOU —
            // validation reads the UTXO set lock-free and is only an early
            // reject; THIS is authoritative. Covers BOTH the plain hot-path and
            // the encrypted `external_delta` path. On a multi-input block that
            // conflicts mid-way, roll back the claims already made so
            // legitimately-unspent inputs are not locked up.
            let mut claimed: Vec<(&str, u32)> = Vec::with_capacity(d.spend.len());
            for (txid, idx) in &d.spend {
                if self.dag.try_mark_spent(txid, *idx) {
                    claimed.push((txid.as_str(), *idx));
                } else {
                    for (t, i) in &claimed {
                        self.dag.unmark_spent(t, *i);
                    }
                    tracing::warn!(
                        "🚫 double-spend rejected on block {}: outpoint {}:{} already spent",
                        &sb.id[..16.min(sb.id.len())],
                        txid,
                        idx
                    );
                    return Ok(PutResult::Rejected(format!(
                        "double-spend: outpoint {txid}:{idx} already spent"
                    )));
                }
            }

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

        // Dédup déjà faite plus haut (avant `apply_diff`) pour garantir
        // l'idempotence sans double-application du delta UTXO. À ce point le
        // bloc est garanti absent du DAG.

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

        // Spent outpoints were already claimed atomically BEFORE `apply_diff`
        // (double-spend guard above), so there is nothing to mark here anymore.

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
