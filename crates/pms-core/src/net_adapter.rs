use crate::crypto::crypto::verify_block_signature;
use crate::validations::apply;
use crate::validations::policy::validate_mint_policy;
use crate::{CoreAdapter, DagRef, validate_block};
use anyhow::Result;
use async_trait::async_trait;
use pms_config::load_config;
use pms_interface::NetDagAdapter;
use pms_network::messages::NetMsg;
use pms_storage::store::PutResult;
use pms_storage::{DagStorage, StoredBlock, UtxoDelta};
use pms_types::{Block, PayloadEnvelope, PlainPayload};
use pms_wire::WireBlock;

#[async_trait]
impl<S> NetDagAdapter for CoreAdapter<S>
where
    S: DagStorage + Send + Sync + 'static,
{
    /// Est‑ce que j’ai déjà ce bloc en RAM ?
    ///
    /// - Sert à court‑circuiter la réception réseau (évite doublons).
    async fn have_block(&self, id: &str) -> bool {
        // rapide: regarde d’abord en RAM
        if self.dag.lock().await.blocks.contains_key(id) {
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
        let policy = &self.policy;

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

        // 1.e) PoW minimal (anti-DOS / anti-spam)
        //
        // On centralise la logique dans validate_wire_block_pow() qui utilise
        // self.policy.min_pow_leading_zero_bits.
        if let Err(e) = self.validate_wire_block_pow(wb) {
            return Ok(PutResult::Rejected(format!("invalid difficulty: {e}")));
        }

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
            // On réutilise les Settings chargés au début de persist_block
            if let Err(e) = validate_mint_policy(outputs, wb, &settings) {
                return Ok(PutResult::Rejected(format!("mint policy violated: {e}")));
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
        // On autorise des blocs "isolés" tant que le DAG n’est pas amorcé,
        // puis on applique min_parents_after_boot dès que > 1 bloc en RAM.
        {
            let dag_was_bootstrapped = { self.dag.lock().await.blocks.len() > 1 };
            if dag_was_bootstrapped && wb.parents.len() < policy.min_parents_after_boot {
                return Ok(PutResult::Rejected(
                    "not enough parents after bootstrap".into(),
                ));
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
        {
            use crate::validate_block;

            let dag = self.dag.lock().await;
            if let Err(e) = validate_block(&dag, &block, policy) {
                // Pour l’instant, le bloc n’est pas encore en RAM ni attaché.
                // On choisit de ne PAS le persister en DAG si la sémantique échoue.
                // (Note : il n’est pas encore dans Rocks non plus, donc rejet propre.)
                return Ok(PutResult::Rejected(format!("dag validation failed: {e}")));
            }
        }

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

                Some(UtxoDelta { spend, create })
            }

            _ => None,
        };

        // Note: append_block_atomic_with_utxo prend Option<&UtxoDelta>
        if !self
            .store
            .append_block_atomic_with_utxo(&sb, delta.as_ref())
            .await?
        {
            return Ok(PutResult::AlreadyExists);
        }

        // ============================================================
        // 6) MISE À JOUR DU DAG EN RAM + FINALITÉ
        // ============================================================
        //
        // Maintenant que RocksDB est en état cohérent, on met à jour le DAG
        // en mémoire et on recalcule la finalité (k-depth, milestones, etc).
        //
        // Attention : on ne fait AUCUN `.await` pendant qu’on tient le lock.
        let (finals_snapshot, last_ms_snapshot) = {
            let mut dag = self.dag.lock().await;

            // On réutilise le `block` déjà validé (header + DAG)
            apply::apply_block_mem(&mut *dag, &block);

            // Met à jour la finalité après insertion
            dag.update_finality_after_insert(&sb.id);

            // Prend un snapshot des blocs finalisés et du dernier milestone
            let finals: Vec<String> = dag.finality.finalized.iter().cloned().collect();
            let last_ms = dag.finality.last_milestone.clone();

            (finals, last_ms)
        };

        // ============================================================
        // 7) PERSISTENCE DE LA FINALITÉ EN STORE (hors lock DAG)
        // ============================================================
        if let Err(e) = self.store.persist_final(&finals_snapshot).await {
            eprintln!("[CoreAdapter] WARN: persist_final failed: {e:#}");
        }

        if let Some(ms) = last_ms_snapshot {
            if let Err(e) = self.store.persist_last_milestone(&ms).await {
                eprintln!("[CoreAdapter] WARN: persist_last_milestone failed: {e:#}");
            }
        }

        // ============================================================
        // 8) Succès global
        // ============================================================
        Ok(PutResult::Inserted)
    }

    /// Diffuse un bloc sur le réseau **si** un serveur est attaché.
    ///
    /// - “Fire‑and‑forget” : si pas de serveur (ex: mode offline), on ne renvoie pas d’erreur.
    async fn broadcast_block(&self, wb: &WireBlock) -> Result<()> {
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
        // 1) essaye le store s’il l’expose
        if let Ok(v) = self.store.top_tips(limit).await {
            if !v.is_empty() {
                return Ok(v);
            }
        }
        // 2) fallback RAM: DAG local
        let mut tips = self.dag.lock().await.find_tips();
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
}
