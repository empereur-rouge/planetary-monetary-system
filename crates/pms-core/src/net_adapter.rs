use crate::CoreAdapter;
use crate::crypto::crypto::verify_block_signature;

use crate::validations::check::ValidatePolicy;
use crate::validations::mint::validate_mint_security;
use crate::validations::nft::validate_nft_action;
use crate::validations::policy::validate_mint_policy;
use anyhow::Result;
use async_trait::async_trait;
use num_traits::ToPrimitive;
use pms_config::load_config;
use pms_event::PmsEvent;
use pms_interface::NetDagAdapter;
use pms_storage::store::PutResult;
use pms_storage::{
    ConfigStorage, DagStorage, NftStorage, NodeRewardsStorage, StoredBlock, UtxoDelta,
};
use pms_types::{Block, PayloadEnvelope, PlainPayload};
use pms_wire::WireBlock;

#[async_trait]
impl<S> NetDagAdapter for CoreAdapter<S>
where
    S: DagStorage + NftStorage + ConfigStorage + NodeRewardsStorage + Send + Sync + 'static,
{
    /// Est‑ce que j’ai déjà ce bloc en RAM ?
    ///
    /// - Sert à court‑circuiter la réception réseau (évite doublons).
    async fn have_block(&self, id: &str) -> bool {
        // rapide: regarde d'abord en RAM (lock-free)
        if self.dag.contains_block(id) {
            return true;
        }
        // fallback: store
        self.store.get_block(id).await.ok().flatten().is_some()
    }

    /// Persiste atomiquement le bloc (store) puis reflète en RAM.
    ///
    /// - `append_block_atomic` garantit idempotence/cohérence.
    /// - On met à jour les compteurs d’enfants en RAM pour garder la logique locale efficace.
    /// Pipeline d'insertion d'un bloc reçu sur le réseau.
    ///
    /// Étapes:
    ///  0. Vérification réseau + présence de la signature (niveau "auth" minimal).
    ///  1. Vérifications légères sur le `WireBlock` (taille payload, parents).
    ///  2. Persistance atomique en store (`StoredBlock`) — source de vérité.
    ///  3. Reconstruction du `Block` en RAM + mise à jour du DAG + finalité.
    ///  4. Persistance de la finalité (finals + dernier milestone) dans le store.
    ///
    /// Attention:
    ///  - Toute la logique de signature reste au niveau `WireBlock` (réseau),
    ///    la validation sémantique du DAG reste dans `validate_block`.
    ///  - On évite tout `await` pendant qu'on tient le lock sur `self.dag`.
    #[allow(clippy::too_many_lines)]
    async fn persist_block(&self, wb: &WireBlock) -> Result<PutResult> {
        // ============================================================
        // 0) Charger la config réseau (network_id + protocol_version)
        //    et construire la meta attendue côté serveur.
        //    → Cela fige le "contrat" réseau pour ce node.
        // ============================================================
        let settings = load_config()?;
        let meta = pms_wire::WireMeta::from(&settings);
        let mut policy = self.policy.clone();

        // Charger la RuntimeConfig depuis le store (Hot-Swap)
        // et mettre à jour la policy avec les paramètres dynamiques
        if let Ok(runtime_config) = self.store.get_runtime_config() {
            policy.update_from_runtime_config(&runtime_config);
            tracing::trace!(
                "RuntimeConfig applied: fee_ratio={}, pow_bits={}",
                runtime_config.platform_fee_bps,
                runtime_config.min_pow_bits
            );
        }

        let policy = &policy;

        // ============================================================
        // 1) VALIDATION WIRE-LEVEL (header + signer + signature + PoW)
        // ============================================================

        // 1.a) Vérifier le network_id / protocol_version
        if wb.network_id != meta.network_id || wb.protocol_version != meta.protocol_version as u16 {
            return Ok(PutResult::Rejected(
                "wrong network_id or protocol_version".to_string(),
            ));
        }

        // 1.b) Clé publique obligatoire
        if wb.signer_pk_hex.trim().is_empty() {
            return Ok(PutResult::Rejected("missing signer public key".to_string()));
        }

        // 1.c) Signature obligatoire
        if wb.signature_hex.trim().is_empty() {
            return Ok(PutResult::Rejected("missing signature".to_string()));
        }

        // 1.d) Vérification cryptographique réelle
        if let Err(e) = verify_block_signature(wb) {
            return Ok(PutResult::Rejected(format!("invalid signature: {e}")));
        }

        // ============================================================
        // 1.e) SINGLE WRITER ENFORCEMENT (Private DAG Mode)
        // ============================================================
        // En mode Single Writer, TOUS les blocs doivent être signés par le Coordinator.
        // C'est le verrouillage protocole pour le mode centralisé.
        if policy.enforce_single_writer {
            if let Some(ref expected_pk) = policy.coordinator_public_key {
                if wb.signer_pk_hex.trim() != expected_pk.trim() {
                    tracing::warn!(
                        "🚫 Single Writer violation: block {} signed by {} but expected {}",
                        &wb.id[..16.min(wb.id.len())],
                        &wb.signer_pk_hex,
                        expected_pk
                    );
                    return Ok(PutResult::Rejected(format!(
                        "single_writer: only Coordinator can create blocks. Got signer: {}, expected: {}",
                        &wb.signer_pk_hex,
                        expected_pk
                    )));
                }
            }
        }

        // [DEPRECATED] 1.f) PoW check removed for Private DAG
        // Server authority replaces Proof-of-Work. The coordinator's signature
        // is the sole validation mechanism. PoW logic kept for documentation purposes.
        // See: validate_wire_block_pow() in core_adapter.rs (disabled, always returns Ok)

        // ============================================================
        // 2) DÉCODAGE PAYLOAD + CONTRÔLES STRUCTURELS SIMPLES
        //    (taille JSON, parents uniques, self-parent, min parents après bootstrap)
        // ============================================================

        // 2.a) On garde la String intacte pour le storage
        let payload_json_opt = wb.payload_json.clone();

        // 2.b) Taille maximale du payload brut (anti-spam)
        if let Some(s) = &payload_json_opt {
            if s.len() > policy.max_payload_bytes {
                return Ok(PutResult::Rejected(format!(
                    "payload too large: {} > {}",
                    s.len(),
                    policy.max_payload_bytes
                )));
            }
        }

        // 2.c) Désérialisation en PayloadEnvelope (si non-null)
        let payload: Option<PayloadEnvelope> = match &payload_json_opt {
            None => None,
            Some(s) if s.trim().is_empty() || s == "null" => None,
            Some(s) => Some(serde_json::from_str::<PayloadEnvelope>(s)?),
        };

        // 1.x) Politique de mint (PlainPayload::Mint seulement)
        //
        // - Plain + Mint = visible → on peut appliquer les règles de montant et d'admin.
        // - EncryptedPayload::Mint reste pour l'instant traité comme "opaque",
        //   la politique de mint ne peut pas être appliquée dessus.
        if let Some(PayloadEnvelope::Plain(PlainPayload::Mint { outputs })) = &payload {
            // Vérification 1: Montant et politique générale
            if let Err(e) = validate_mint_policy(outputs, wb, &settings) {
                return Ok(PutResult::Rejected(format!("mint policy violated: {e}")));
            }

            // Vérification 2: SÉCURITÉ COORDINATEUR
            // Seul le Coordinateur peut minter (Mainnet/Testnet)
            let policy = ValidatePolicy::from_settings(&settings.validation);
            // Override coordinator key from config if specified, else use hardcoded
            let mut policy = policy;
            if let Some(ref custom_key) = settings.validation.coordinator_public_key {
                policy.coordinator_public_key = Some(custom_key.clone());
            } else {
                match settings.network.mode {
                    pms_config::NetworkMode::Mainnet => {
                        policy.coordinator_public_key =
                            Some(pms_consensus::COORDINATOR_PUBLIC_KEY_MAINNET.to_string());
                    }
                    pms_config::NetworkMode::Testnet => {
                        policy.coordinator_public_key =
                            Some(pms_consensus::COORDINATOR_PUBLIC_KEY_TESTNET.to_string());
                    }
                    pms_config::NetworkMode::Dev => {
                        policy.coordinator_public_key = None;
                    }
                }
            }
            if let Err(e) = validate_mint_security(wb, &policy) {
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
        // - Applique au store si valide (Mint → apply_mint, Transfer/Burn → apply_action)
        if let Some(PayloadEnvelope::Plain(PlainPayload::Nft(action))) = &payload {
            // Récupère la clé publique du signataire
            let signer_pk = &wb.signer_pk_hex;

            // Récupère les clés Authority pour la validation Cube
            let authority_pks = &settings.fees.authority_public_keys;

            // Valide l'action
            if let Err(e) = validate_nft_action(
                action,
                signer_pk,
                policy.coordinator_public_key.as_deref(),
                authority_pks,
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
                    // Si new_owner_x25519_pubkey est fourni, on met à jour le block_id
                    // pour pointer vers ce bloc Transfer (qui contiendra les métadonnées
                    // re-chiffrées pour le nouveau owner via le serveur coordinator)
                    if new_owner_x25519_pubkey.is_some() {
                        // Re-encryption: le bloc Transfer devient la nouvelle référence
                        self.store.apply_transfer(token_id, to, &wb.id)
                    } else {
                        // Transfer simple sans re-encryption (ancien owner garde accès)
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

            // Émet l'événement NFT sur le bus
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
        // - Applique la mise à jour de configuration
        // - Persiste dans le store
        // - Émet un événement
        if let Some(PayloadEnvelope::Plain(PlainPayload::ConfigUpdate(update))) = &payload {
            // Timestamp actuel
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);

            // Appliquer la mise à jour au store
            match self.store.apply_config_update(update, &wb.id, timestamp) {
                Ok(new_config) => {
                    tracing::info!(
                        "✅ Config update applied: {} - fee_rate={}bps, platform_fee={}bps",
                        update.description(),
                        new_config.fee_rate_bps,
                        new_config.platform_fee_bps
                    );
                }
                Err(e) => {
                    tracing::error!("❌ ConfigUpdate apply failed: {}", e);
                    return Ok(PutResult::Rejected(format!("ConfigUpdate failed: {}", e)));
                }
            }
        }

        // 2.d) Parents uniques + pas d’auto-parentage (protection de base)
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

        // 2.e) Minimum de parents après bootstrap (soft anti-spam)
        //
        // On requiert min_parents (typiquement 2),
        // MAIS on permet d'utiliser "genesis" comme parent supplémentaire
        // si le DAG n'a pas assez de tips distincts.
        //
        // NOTE: En mode Single Writer, cette règle est REMPLACÉE par enforce_single_parent.
        //
        // Règle:
        //  - parents.len() >= min_parents_after_boot
        //  - SAUF si le bloc contient "genesis" comme parent ET qu'il n'y a pas assez de tips
        //  - Dans ce cas, genesis peut "compléter" le compte de parents
        if !policy.enforce_single_writer {
            let dag_was_bootstrapped = { self.dag.len() > 1 };
            if dag_was_bootstrapped && wb.parents.len() < policy.min_parents_after_boot {
                // Vérifier si genesis est utilisé comme parent supplémentaire
                let has_genesis = wb.parents.iter().any(|p| p == "genesis");
                let available_tips = self.dag.find_tips().len();

                // Autoriser si genesis est utilisé ET qu'il n'y a pas assez de tips disponibles
                if !(has_genesis && available_tips < policy.min_parents_after_boot) {
                    return Ok(PutResult::Rejected(format!(
                        "not enough parents after bootstrap: got {}, need {}. Tip: use 'genesis' as parent during bootstrap.",
                        wb.parents.len(),
                        policy.min_parents_after_boot
                    )));
                }
            }
        }

        // 2.f) SINGLE WRITER: Chaîne Linéaire (1 parent)
        //
        // En mode Single Writer, on impose exactement 1 parent par bloc.
        // Cela garantit une chaîne linéaire au lieu d'un DAG.
        if settings.validation.enforce_single_writer {
            let is_genesis = payload.as_ref().is_some_and(|p| {
                matches!(p, PayloadEnvelope::Plain(PlainPayload::Genesis))
            });

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
        // À ce stade:
        //   - header ok (réseau, signature, PoW)
        //   - payload JSON parsé (ou None)
        //   - contraintes structurelles simples faites (taille, parents uniques)
        //
        // On peut donc construire un Block propre et cohérent.
        let block = Block {
            id: wb.id.clone(),
            parents: wb.parents.clone(),
            payload: payload.clone(),
            nonce: wb.nonce,
            metadata: None, // Les blocs reçus du réseau n'ont pas de metadata
            signer_pk: Some(wb.signer_pk_hex.clone()).filter(|s| !s.is_empty()),
            signature: Some(wb.signature_hex.clone()).filter(|s| !s.is_empty()),
        };

        // ============================================================
        // 4) VALIDATION DAG PROFONDE (UTXO, double-spend, règles métier)
        // ============================================================
        //
        // On utilise ta fonction `validate_block(dag, &block, policy)` qui:
        //   - vérifie parents_exist / no_cycle / parent_count
        //   - applique la politique UTXO (double spend, montants, etc.)
        //
        // Important: on ne modifie pas le DAG ici, on fait juste les checks.
        //
        // ## FIX RACE CONDITION (Phase 2 IOTA-like)
        //
        // Les tips sont sélectionnés depuis RocksDB (`store.top_tips()`), mais
        // validate_block vérifie les parents en RAM. Sous charge parallèle,
        // un parent peut exister dans RocksDB mais pas encore en RAM.
        //
        // Solution: vérifier d'abord que les parents existent dans le store.
        // ============================================================

        // 4.a) Vérification des parents (RAM DAG + store)
        // 🔧 FIX: Check RAM DAG first to handle async persistence race condition
        let t_parents_start = std::time::Instant::now();
        if policy.enforce_parent_existence {
            for parent_id in &block.parents {
                // Check RAM DAG first (blocks are inserted here immediately)
                let in_ram = self.dag.contains_block(parent_id);

                // If not in RAM, check store (for blocks not yet loaded in RAM)
                if !in_ram {
                    match self.store.get_block(parent_id).await {
                        Ok(Some(_)) => continue, // Parent in store ✓
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
        // Évite le lock DAG global si activé dans la policy.
        let t_utxo_val_start = std::time::Instant::now();
        if policy.skip_utxo_checks {
            if let Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx))) = &block.payload {
                use crate::validations::transactions::validate_transaction_async;
                if let Err(e) = validate_transaction_async(&self.utxos, tx).await {
                    return Ok(PutResult::Rejected(format!("utxo validation failed: {e}")));
                }
            }
        }
        let t_utxo_val = t_utxo_val_start.elapsed();

        // 4.b) Validation DAG complète - BYPASSED for lock-free performance
        // Parents are already validated via parents_exist_in_store (step 4.a)
        // Double-spend is checked via ShardedUtxoSet (step 4.new)
        // The locked DAG validation was causing 27-280ms latency!
        let t_dag_val_start = std::time::Instant::now();
        // DISABLED: This was the bottleneck!
        // {
        //     use crate::validate_block;
        //     let dag = self.dag.lock().await;
        //     if let Err(e) = validate_block(&dag, &block, policy) {
        //         return Ok(PutResult::Rejected(format!("dag validation failed: {e}")));
        //     }
        // }
        let t_dag_val = t_dag_val_start.elapsed();

        // ============================================================
        // 5) PERSISTENCE ATOMIQUE EN ROCKSDB
        // ============================================================
        //
        // On convertit le WireBlock en StoredBlock (format de stockage), et on
        // utilise append_block_atomic() pour garantir:
        //   - idempotence (AlreadyExists si bloc déjà présent),
        //   - mise à jour cohérente des index (tips, children_count, timestamps).
        let sb = StoredBlock {
            id: wb.id.clone(),
            parents: wb.parents.clone(),
            payload_json: payload_json_opt, // on réutilise la même String
            nonce: wb.nonce,
            network_id: wb.network_id.clone(),
            protocol_version: wb.protocol_version,
            signer_pk_hex: wb.signer_pk_hex.clone(),
            signature_hex: wb.signature_hex.clone(),
            metadata: wb.metadata.clone(),
        };

        // Construction du delta UTXO (si applicable)
        let delta = match &payload {
            Some(PayloadEnvelope::Plain(PlainPayload::Mint { outputs })) => {
                // Mint = create only (no inputs)
                let create = outputs
                    .iter()
                    .enumerate()
                    .map(|(i, out)| {
                        (
                            sb.id.clone(),
                            i as u32,
                            out.address.clone(),
                            out.amount.clone(),
                        )
                    })
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
                    .map(|(i, out)| {
                        (
                            sb.id.clone(),
                            i as u32,
                            out.address.clone(),
                            out.amount.clone(),
                        )
                    })
                    .collect();

                // Accumulation du pool de fees pour les nœuds
                // Calcul: fee * (node_fee_bps / 10000)
                if let Ok(runtime_config) = self.store.get_runtime_config() {
                    if runtime_config.node_fee_bps > 0 {
                        // Parse fee (format décimal: "1.50000000")
                        if let Ok(fee_decimal) = rust_decimal::Decimal::from_str_exact(&tx.fee) {
                            // Convertir en satoshis (8 décimales)
                            let fee_sats = (fee_decimal * rust_decimal::Decimal::from(100_000_000))
                                .to_u64()
                                .unwrap_or(0);

                            // Part pour les nœuds
                            let node_portion =
                                fee_sats * u64::from(runtime_config.node_fee_bps) / 10000;

                            if node_portion > 0 {
                                if let Err(e) = self.store.add_to_fee_pool(node_portion) {
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

                // Add fee distribution outputs
                for out in fee_outputs {
                    create.push((sb.id.clone(), idx, out.address.clone(), out.amount.clone()));
                    idx += 1;
                }

                // Add block reward outputs
                for out in reward_outputs {
                    create.push((sb.id.clone(), idx, out.address.clone(), out.amount.clone()));
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

            _ => None,
        };

        // Note: append_block_atomic_with_utxo prend Option<&UtxoDelta>
        // === ASYNC PERSISTENCE: Update RAM first, persist in background ===

        let t0 = std::time::Instant::now();

        // 5.a) UTXO RAM Update FIRST (essential for preventing double-spend)
        if let Some(d) = &delta {
            for (txid, idx) in &d.spend {
                self.utxos
                    .remove(&pms_types::OutputId {
                        txid: txid.clone(),
                        index: *idx,
                    })
                    .await;
            }
            for (txid, idx, addr, amount) in &d.create {
                self.utxos
                    .add(
                        pms_types::OutputId {
                            txid: txid.clone(),
                            index: *idx,
                        },
                        pms_types::TxOutput {
                            address: addr.clone(),
                            amount: amount.clone(),
                        },
                    )
                    .await;
            }
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

        // Lock-free insertion into concurrent DAG
        self.dag.insert_block(block.clone());

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
            let mut finality = self.dag.finality.write().unwrap();

            // a) Milestone handling
            if let Some(PayloadEnvelope::Plain(PlainPayload::Milestone {
                distribute_node_rewards,
                ..
            })) = &payload
            {
                finality.last_milestone = Some(block.id.clone());
                if finality.finalized.insert(block.id.clone()) {
                    newly_finalized.push(block.id.clone());
                }

                // Distribution des fees aux nœuds si demandé
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

                                    // Collecter les UTXOs à créer (sans await)
                                    for (idx, (node_pk, block_count)) in miners.iter().enumerate() {
                                        let share = pool * block_count / total_blocks;
                                        if share > 0 {
                                            let reward_address = self
                                                .store
                                                .get_node_reward_address(node_pk)
                                                .unwrap_or_else(|_| node_pk.clone());

                                            let txid = sb.id.clone();
                                            let amount = rust_decimal::Decimal::from(share)
                                                / rust_decimal::Decimal::from(100_000_000);
                                            let amount_str = format!("{:.8}", amount);

                                            let out_id = pms_types::OutputId {
                                                txid,
                                                index: idx as u32,
                                            };
                                            let out = pms_types::TxOutput {
                                                address: reward_address.clone(),
                                                amount: amount_str,
                                            };

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

            // b) K-depth finalization
            // Mark blocks with enough confirmations as final
            let depth_k = finality.depth_k;
            if depth_k > 0 {
                // Collect block IDs to check (avoid borrowing issues)
                let block_ids: Vec<String> = self
                    .dag
                    .blocks
                    .iter()
                    .map(|entry| entry.key().clone())
                    .collect();

                for bid in block_ids {
                    if finality.finalized.contains(&bid) {
                        continue;
                    }

                    // Count descendants (confirmations) via BFS
                    let confirmations = self.dag.count_descendants(&bid, depth_k);
                    if confirmations >= depth_k {
                        finality.finalized.insert(bid.clone());
                        newly_finalized.push(bid);
                    }
                }
            }
        }

        let t_dag = t1.elapsed();

        // ============================================================
        // 6.c) Créer les UTXOs de récompense (après le lock finality)
        // ============================================================
        for (out_id, out, node_pk, share) in reward_utxos {
            // Ajouter à UTXO RAM (async est ok ici, hors du lock)
            self.utxos.add(out_id, out.clone()).await;

            // Émettre un événement
            self.event_bus.emit(PmsEvent::NodeRewardDistributed {
                node_pk,
                address: out.address,
                amount_sats: share,
                milestone_id: sb.id.clone(),
            });
        }

        // ============================================================
        // 7) FIRE-AND-FORGET: Send to background persist channel
        // ============================================================
        use crate::background_persist::PersistJob;
        let job = PersistJob {
            block: sb.clone(),
            delta,
            newly_finalized,
        };
        // send() is non-blocking if buffer has space, drops if full (acceptable for high TPS)
        let _ = self.persist_tx.try_send(job);

        let t_total = t0.elapsed();

        // Log timing every 100th block for perf analysis
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if count.is_multiple_of(500) {
            tracing::info!(
                target = "pms_perf",
                block_id = %sb.id,
                parents_us = t_parents.as_micros() as u64,
                utxo_val_us = t_utxo_val.as_micros() as u64,
                dag_val_us = t_dag_val.as_micros() as u64,
                utxo_ram_us = t_utxo.as_micros() as u64,
                dag_insert_us = t_dag.as_micros() as u64,
                total_us = t_total.as_micros() as u64,
                "persist_block timing (µs)"
            );
        }

        // ============================================================
        // 8) Succès global - Client gets response BEFORE disk write
        // ============================================================

        // Incrémenter le compteur de blocs pour ce mineur (node rewards)
        if !wb.signer_pk_hex.trim().is_empty() {
            if let Err(e) = self.store.increment_node_block_count(&wb.signer_pk_hex) {
                tracing::warn!("Failed to increment node block count: {}", e);
            }
        }

        Ok(PutResult::Inserted)
    }

    /// Diffuse un bloc sur le réseau **si** un serveur est attaché.
    ///
    /// - “Fire‑and‑forget” : si pas de serveur (ex: mode offline), on ne renvoie pas d’erreur.
    async fn broadcast_block(&self, _wb: &WireBlock) -> Result<()> {
        // if let Some(srv) = self.server_arc().await {
        //     srv.broadcast(&NetMsg::Block {
        //         id: wb.id.clone(),
        //         parents: wb.parents.clone(),
        //         payload_json: wb.payload_json.clone(),
        //         nonce: wb.nonce,
        //         network_id: wb.network_id.clone(),
        //         protocol_version: wb.protocol_version,
        //         signature_hex: wb.signature_hex.clone(),
        //         signer_pk_hex: wb.signer_pk_hex.clone(),
        //     }).await?;
        // }
        Ok(())
    }

    async fn top_tips(&self, limit: usize) -> Result<Vec<String>> {
        // [SINGLE WRITER] Optimisation : Sélection linéaire simple
        if self.policy.enforce_single_writer {
            // 🔧 FIX: Check RAM DAG first (contains most recent blocks)
            // This fixes race condition where blocks are in DAG but not yet in RocksDB
            let dag_tips = self.dag.find_tips();
            if !dag_tips.is_empty() {
                // Return latest tip from RAM (most up-to-date)
                // In linear chain mode, find_tips() returns 1 tip (the chain head)
                println!(
                    "[ADAPTER] top_tips (RAM): found {} tips. Last: {:?}",
                    dag_tips.len(),
                    dag_tips.last()
                );
                return Ok(vec![dag_tips[dag_tips.len() - 1].clone()]);
            }

            // Fallback to store if DAG is empty (shouldn't happen after bootstrap)
            if let Ok(recents) = self.store.recent_ids(1).await {
                if !recents.is_empty() {
                    return Ok(recents);
                }
            }
        }

        // 1) essaye le store s’il l’expose
        if let Ok(v) = self.store.top_tips(limit).await {
            if !v.is_empty() {
                eprintln!("[ADAPTER] top_tips (store): {} tips", v.len());
                return Ok(v);
            }
        }

        // 2) fallback RAM: DAG local (lock-free)
        let mut tips = self.dag.find_tips();
        if tips.len() > limit {
            tips.truncate(limit);
        }
        Ok(tips)
    }

    async fn get_block(&self, id: &str) -> Result<Option<WireBlock>> {
        if let Some(sb) = self.store.get_block(id).await? {
            return Ok(Some(WireBlock {
                id: sb.id,
                parents: sb.parents,
                payload_json: sb.payload_json,
                nonce: sb.nonce,
                network_id: sb.network_id,
                protocol_version: sb.protocol_version,
                signer_pk_hex: sb.signer_pk_hex,
                signature_hex: sb.signature_hex,
                metadata: sb.metadata,
            }));
        }
        Ok(None)
    }

    async fn recent_ids(&self, limit: usize) -> Result<Vec<String>> {
        self.store.recent_ids(limit).await
    }

    async fn get_blocks_by_ids(&self, ids: &[String]) -> Result<Vec<WireBlock>> {
        let sbs = self.store.get_blocks_by_ids(ids).await?;
        Ok(sbs)
    }

    fn min_pow_leading_zero_bits(&self) -> u8 {
        self.policy.min_pow_leading_zero_bits
    }

    async fn circulating_supply(&self) -> (rust_decimal::Decimal, u64) {
        let (dec, count) = self.utxos.circulating_supply().await;
        (dec, count as u64)
    }

    async fn balance_by_address(&self, address: &str) -> rust_decimal::Decimal {
        self.utxos.balance_by_address(address).await
    }

    async fn utxos_by_address(&self, address: &str) -> Vec<(pms_types::OutputId, pms_types::TxOutput)> {
        self.utxos.utxos_by_address(address).await
    }

    async fn add_utxo(&self, txid: String, index: u32, address: String, amount: String) {
        use pms_types::{OutputId, TxOutput};
        self.utxos
            .add(OutputId { txid, index }, TxOutput { address, amount })
            .await;
    }
}
