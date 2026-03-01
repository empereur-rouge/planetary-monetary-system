use crate::block_builder::BlockMineBuilder;
use crate::finality::FinalityState;
use crate::has_k_confirmations_dag;
use crate::validations::check::ValidatePolicy;
use crate::validations::{apply, check};
use anyhow::Result;
use pms_interface::NetDagAdapter;
use pms_storage::{DagStorage, StoredBlock};
use pms_types::{Block, BlockId, PayloadEnvelope, PlainPayload};
use pms_utils::{compute_block_id, hash_meets_difficulty};
use pms_wallet::SignerBackend;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::{WireBlock, WireMeta};
use rand::Rng;
use rand::distr::Distribution;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

pub const TIP_CHILDREN_THRESHOLD: u64 = 4;
pub const MAX_TIPS_CAP: usize = 64;
pub const PARENTS_MIN: usize = 2;
pub const PARENTS_MAX: usize = 2;

pub struct Dag {
    pub blocks: HashMap<BlockId, Block>, // consensus: id, parents, payload?, nonce
    pub children: HashMap<BlockId, u64>, // NON-consensus: children_count (local)
    pub children_idx: HashMap<BlockId, Vec<BlockId>>,
    pub net_adapter: Option<Arc<dyn pms_network::adapter::DagAdapter>>,
    pub finality: FinalityState,
    pub spent_outpoints: HashSet<(String, u32)>,
}

use crate::concurrent_dag::ConcurrentDag;
pub type DagRef = Arc<ConcurrentDag>;

impl Dag {
    pub fn new_with_genesis(genesis: Block) -> Self {
        let mut dag = Self {
            blocks: HashMap::new(),
            children: HashMap::new(),
            children_idx: HashMap::new(),
            net_adapter: None,
            finality: FinalityState::default(),
            spent_outpoints: HashSet::new(),
        };
        dag.children.insert(genesis.id.clone(), 0);
        dag.blocks.insert(genesis.id.clone(), genesis);
        dag
    }

    /// Ajoute un block déjà validé (parents présents, nonce/payload ok).
    pub fn add_block_unchecked(&mut self, block: Block) -> Result<()> {
        if self.blocks.contains_key(&block.id) {
            return Err(anyhow::anyhow!("Block déjà existant"));
        }
        for p in &block.parents {
            if !self.blocks.contains_key(p) {
                return Err(anyhow::anyhow!("Parent manquant: {p}"));
            }
        }
        // MAJ compteur enfants (local)
        self.blocks.insert(block.id.clone(), block);
        Ok(())
    }

    pub fn add_block(&mut self, block: Block) -> Result<()> {
        if self.blocks.contains_key(&block.id) {
            return Err(anyhow::anyhow!("Block déjà existant"));
        }
        for p in &block.parents {
            if !self.blocks.contains_key(p) {
                return Err(anyhow::anyhow!("Parent manquant: {p}"));
            }
        }

        // ✅ Validation complète
        check::validate_block(self, &block, &ValidatePolicy::default())
            .map_err(anyhow::Error::msg)?;

        // ✅ Application en RAM: children, spent_outpoints, insertion, finalité
        apply::apply_block_mem(self, &block);

        Ok(())
    }

    /// Sélectionne [min_parents..=max_parents] parents parmi les tips (tirage pondéré).
    /// - Bootstrapping : s’il n’y a que le genesis, on retourne [genesis].
    pub fn select_parents(&self, min_parents: usize, max_parents: usize) -> Vec<BlockId> {
        // 0) Bootstrapping : uniquement le genesis présent
        if self.blocks.len() == 1 {
            if let Some((gid, _b)) = self.blocks.iter().find(|(_, b)| b.parents.is_empty()) {
                return vec![gid.clone()];
            }
        }

        // 1) Récupère les tips
        let mut tips = self.find_tips();

        // Fallback défensif si `find_tips()` est vide
        if tips.is_empty() {
            if let Some((gid, _b)) = self.blocks.iter().find(|(_, b)| b.parents.is_empty()) {
                return vec![gid.clone()]; // genesis
            }
            if let Some((any, _)) = self.blocks.iter().next() {
                return vec![any.clone()];
            }
            return vec![]; // DAG vide
        }

        // 2) Détermine k dans [min..=max], borné par le nb de tips
        let mut rng = rand::rng();
        let span = max_parents.saturating_sub(min_parents);
        let mut k = min_parents
            + if span == 0 {
                0
            } else {
                rng.random_range(0..=span)
            };
        k = k.min(tips.len());

        // 3) Tirage pondéré sur les poids cumulatifs
        let cw = self.cumulative_weights();
        let mut pool: Vec<(BlockId, u64)> = tips
            .drain(..)
            .map(|id| {
                let w = *cw.get(&id).unwrap_or(&1);
                (id, if w == 0 { 1 } else { w })
            })
            .collect();

        let mut selected = Vec::with_capacity(k);
        for _ in 0..k {
            if pool.is_empty() {
                break;
            }
            if pool.len() == 1 {
                // SAFETY: On a vérifié que pool.len() == 1, donc pop() retourne Some
                if let Some((id, _)) = pool.pop() {
                    selected.push(id);
                }
                break;
            }
            let weights: Vec<u64> = pool.iter().map(|(_, w)| *w).collect();
            let all_equal = weights.windows(2).all(|w| w[0] == w[1]);

            let idx = if all_equal {
                rand::Rng::random_range(&mut rng, 0..pool.len())
            } else {
                let dist =
                    rand::distr::weighted::WeightedIndex::new(&weights).expect("poids invalides");
                dist.sample(&mut rng)
            };

            selected.push(pool.swap_remove(idx).0);
        }

        selected
    }

    /// Construit un block (id/nonce déjà calculés, payload clair ou chiffré) et l’ajoute.
    /// La validation (nonce, payload signatures, UTXO…) doit être faite AVANT.
    pub fn add_payload_auto_parents_mined(
        &mut self,
        payload: Option<PayloadEnvelope>,
        difficulty_leading_zeros: u8, // 0 en tests
        compute_id: impl Fn(&[String], &Option<PayloadEnvelope>, u64) -> BlockId,
    ) -> Result<Block> {
        let parents = self.select_parents(PARENTS_MIN, PARENTS_MAX);
        if parents.is_empty() {
            return Err(anyhow::anyhow!("Aucun parent disponible"));
        }

        let block = BlockMineBuilder::new(parents, payload, compute_id, |id| {
            self.blocks.contains_key(id)
        })
        .difficulty(difficulty_leading_zeros)
        .canonicalize_parents(true)
        .build();

        self.add_block(block.clone()).map_err(anyhow::Error::msg)?;

        self.update_finality_after_insert(&block.id);
        Ok(block)
    }

    /// Exporte tout le DAG en JSON (backup complet).
    /// - `confidential = true` => payload_type est masqué
    /// - `confidential = false` => payload_type visible
    pub fn export_json(&self) -> Value {
        let blocks: Vec<Value> = self
            .blocks
            .values()
            .map(|b| {
                let payload_json = match &b.payload {
                    Some(PayloadEnvelope::Encrypted(ep)) => {
                        json!({
                            "scheme": ep.scheme,
                            "key_version": ep.key_version,
                            "aad": { "len_hint": ep.aad.len_hint },
                            "commitment": ep.commitment,
                            "ciphertext_b64": ep.ciphertext_b64,
                            "recipients": ep.recipients,
                            "nonce_b64": ep.nonce_b64,
                        })
                    }
                    Some(PayloadEnvelope::Plain(pp)) => {
                        json!(pp)
                    }
                    None => json!(null),
                };

                json!({
                    "id": b.id,
                    "parents": b.parents,
                    "nonce": b.nonce,
                    "payload": payload_json,
                })
            })
            .collect();

        json!({
            "blocks": blocks,
        })
    }

    /// Variante "persistante" : mine le bloc, le persiste de façon atomique dans Redis,
    /// puis met à jour l'état en mémoire si la persistance a réussi.
    ///
    /// - `store` : back-end Redis (ou autre) déjà initialisé
    /// - `difficulty_leading_zeros` : PoW (0 en tests)
    /// - `compute_id` : ton hash canonique
    pub async fn add_payload_auto_parents_mined_persist(
        &mut self,
        payload: Option<PayloadEnvelope>,
        difficulty_leading_zeros: u8,
        meta: &WireMeta,
        signer_pk_hex: String,
        signer: &impl SignerBackend,
        compute_id: impl Fn(&[String], &Option<PayloadEnvelope>, u64) -> BlockId,
        adapter: &Arc<dyn NetDagAdapter>,
    ) -> anyhow::Result<Block> {
        // 1) Parents
        let min = if self.blocks.len() <= 1 {
            1
        } else {
            PARENTS_MIN
        };

        // Tips en RAM
        let mut parents = self.find_tips();

        // borne max
        if parents.len() > 256 {
            parents.truncate(256);
        }

        // Si pas assez de parents, on fallback sur le genesis
        if parents.len() < min {
            if let Some((gid, _b)) = self.blocks.iter().find(|(_, b)| b.parents.is_empty()) {
                parents.push(gid.clone());
            }
        }

        // tri + dédup pour la stabilité
        parents.sort();
        parents.dedup();

        // 2) Mine local
        let block = BlockMineBuilder::new(parents.clone(), payload.clone(), compute_id, |id| {
            self.blocks.contains_key(id)
        })
        .difficulty(difficulty_leading_zeros)
        .canonicalize_parents(true)
        .build();

        // 3) JSON du payload
        let payload_json = serde_json::to_string(&block.payload)?;

        // 4) WireBlock unsigned
        let mut unsigned = WireBlock {
            id: block.id.clone(),
            parents: block.parents.clone(),
            payload_json: Some(payload_json),
            nonce: block.nonce,

            // méta réseau (prod)
            network_id: meta.network_id.clone(),
            protocol_version: meta.protocol_version as u16,
            signer_pk_hex: signer_pk_hex.clone(),
            signature_hex: String::new(),
            metadata: block.metadata.clone(),
        };

        // 5) Canonical message
        let msg = canonical_wireblock_message(&unsigned);

        // 6) Signature
        unsigned.signature_hex = signer
            .sign(&msg)
            .map_err(|e| anyhow::anyhow!("sign error: {e:?}"))?;

        // 7) Persistance via adapter (= pipeline prod)
        adapter.persist_block(&unsigned).await?;

        // 8) Miroir RAM
        apply::apply_block_mem(self, &block);
        self.update_finality_after_insert(&block.id);

        Ok(block)
    }

    /// Crée un DAG vide (utile pour bootstrap).
    pub fn new_empty() -> Self {
        Self {
            blocks: HashMap::new(),
            children: HashMap::new(),
            children_idx: HashMap::new(),
            net_adapter: None,
            finality: FinalityState::default(),
            spent_outpoints: HashSet::new(),
        }
    }

    /// Recharge tout l’état depuis le store (sans recalcul d’ID).
    /// Hypothèse: le store contient des `StoredBlock` cohérents.
    /// Reconstruit un DAG en mémoire à partir d’un store (Redis).
    pub async fn bootstrap_from_store<S>(store: &S) -> Result<Self>
    where
        S: DagStorage + Send + Sync,
    {
        // 1) Récupère tous les ids
        let mut ids = store.all_block_ids().await.map_err(anyhow::Error::msg)?;
        ids.sort();

        let mut dag = Dag {
            blocks: Default::default(),
            children: Default::default(),
            children_idx: HashMap::new(),
            net_adapter: None,
            finality: FinalityState::default(),
            spent_outpoints: HashSet::new(),
        };

        // 2) Charge chaque bloc, désérialise et insère
        for id in ids {
            // On récupère le StoredBlock complet (avec meta + signature),
            // mais on ne garde en RAM que ce qui intéresse le DAG.
            let Some(sb) = store.get_block(&id).await.map_err(anyhow::Error::msg)? else {
                continue;
            };

            let payload: Option<PayloadEnvelope> = match sb.payload_json {
                Some(s) => {
                    let t = s.trim();
                    if t.is_empty() || t == "null" {
                        None
                    } else {
                        Some(serde_json::from_str::<PayloadEnvelope>(t).map_err(|e| {
                            anyhow::anyhow!("bootstrap parse payload for {}: {e}", id)
                        })?)
                    }
                }
                None => None,
            };

            let b = Block {
                id: sb.id.clone(),
                parents: sb.parents.clone(),
                payload,
                nonce: sb.nonce,
                metadata: None,
                signer_pk: Some(sb.signer_pk_hex).filter(|s| !s.is_empty()),
                signature: Some(sb.signature_hex).filter(|s| !s.is_empty()),
            };

            // incrémente les compteurs enfants (en mémoire)
            for p in &sb.parents {
                *dag.children.entry(p.clone()).or_default() += 1;
            }
            dag.blocks.insert(sb.id, b);
        }

        // Recharge finalité depuis store
        let finals = store.load_final().await.unwrap_or_default();
        dag.finality.finalized.extend(finals.into_iter());
        dag.finality.last_milestone = store.load_last_milestone().await.unwrap_or(None);

        // 🔑 Reconstruit l’index parent -> enfants
        dag.children_idx.clear();
        for b in dag.blocks.values() {
            for p in &b.parents {
                dag.children_idx
                    .entry(p.clone())
                    .or_default()
                    .push(b.id.clone());
            }
        }

        Ok(dag)
    }

    /// Charge le DAG depuis le store ; s’il est vide, crée un genesis,
    /// le persiste de façon atomique, puis retourne un DAG initialisé.
    pub async fn bootstrap_from_store_or_new_dag<S>(store: &S, meta: &WireMeta) -> Result<Self>
    where
        S: DagStorage + Send + Sync,
    {
        let mut ids = store.all_block_ids().await?;
        if ids.is_empty() {
            // 1) Construire un bloc genesis (payload plain Genesis)
            let genesis = Block::genesis(compute_block_id);

            // 2) Sérialiser son payload
            let payload_json = serde_json::to_string(&genesis.payload).ok();

            // 3) Créer un StoredBlock complet (prod)
            let sb = StoredBlock {
                id: genesis.id.clone(),
                parents: genesis.parents.clone(),
                payload_json,
                nonce: genesis.nonce,

                // méta réseau cohérente avec le reste
                network_id: meta.network_id.clone(),
                protocol_version: meta.protocol_version as u16,
                // bloc système: on le marque comme GENESIS
                signer_pk_hex: "GENESIS".to_string(),
                signature_hex: String::new(),
                metadata: genesis.metadata.clone(),
            };

            // idempotent côté store (append_atomic retourne false si déjà présent)
            let _ = store.append_block_atomic(&sb).await?;

            // 4) Retourner un DAG contenant le genesis
            return Ok(Dag::new_with_genesis(genesis));
        }

        // Store non vide → on garde le chemin existant
        ids.sort();
        Self::bootstrap_from_store(store).await
    }

    /// Lit les tips “fraîches” depuis Redis (si dispo), sinon fallback local.
    pub async fn tips_from_store_or_local<S>(&self, store: &S, limit: usize) -> Vec<BlockId>
    where
        S: DagStorage + Send + Sync,
    {
        match store.top_tips(limit).await {
            Ok(v) if !v.is_empty() => v,
            _ => self.find_tips(), // fallback
        }
    }

    // FINALITY //
    /// Vrai si ce bloc est finalisé (atteint par le dernier milestone valide).
    pub fn is_final(&self, id: &str) -> bool {
        self.finality.is_final(id)
    }

    /// À appeler après ajout d’un bloc. Si `block.payload` est un `Milestone`
    /// **valide** (signature vérifiée ailleurs), on finalise tout son cône d’ancêtres.
    pub fn maybe_update_finality_with(&mut self, block: &Block) {
        match &block.payload {
            Some(PayloadEnvelope::Plain(PlainPayload::Milestone { approved, .. })) => {
                // 1) on marque le milestone
                self.finality.last_milestone = Some(block.id.clone());
                // 2) on finalise tous les parents accessibles (BFS)
                let mut q: VecDeque<&String> = approved.iter().collect();
                while let Some(pid) = q.pop_front() {
                    if self.finality.finalized.insert(pid.clone()) {
                        if let Some(pb) = self.blocks.get(pid) {
                            for gp in &pb.parents {
                                q.push_back(gp);
                            }
                        }
                    }
                }
                // 3) finalise le milestone lui-même
                self.finality.finalized.insert(block.id.clone());
            }
            _other => {
                tracing::trace!(block_id = %block.id, "maybe_update_finality_with: non-Milestone payload, skipping");
            }
        }
    }

    /// Appelée après ajout d'un bloc pour mettre à jour la finalité.
    pub fn update_finality_after_insert(&mut self, new_block_id: &str) {
        let k = self.finality.depth_k;

        // 1) Si le nouveau bloc est un milestone, on met à jour la seed AVANT le calcul K-depth
        if let Some(b) = self.blocks.get(new_block_id) {
            if matches!(
                &b.payload,
                Some(PayloadEnvelope::Plain(PlainPayload::Milestone { .. }))
            ) {
                self.finality.set_milestone(new_block_id.to_string());
            }
        }

        // 2) K-depth
        if k == 0 {
            return;
        }

        // On clone les clés pour éviter les problèmes d’emprunt si tu modifies la map
        let ids: Vec<_> = self.blocks.keys().cloned().collect();
        for id in ids {
            if !self.finality.is_final(&id) && has_k_confirmations_dag(self, &id, k) {
                self.finality.mark_final(&id);
            }
        }
    }

    /// Sélectionne les parents et forge un bloc, sans modifier le DAG.
    pub fn forge_block<F>(
        &self,
        payload: Option<PayloadEnvelope>,
        difficulty: u32,
        mut compute_id: F,
    ) -> Block
    where
        F: FnMut(&WireBlock) -> String,
    {
        let parents = {
            let mut tips = self.find_tips();
            tips.sort();
            tips
        };

        let payload_json = payload.as_ref().and_then(|p| serde_json::to_string(p).ok());

        let mut nonce: u64 = 0;

        loop {
            // On reconstruit un WireBlock "nu" à chaque itération.
            // network_id / protocol_version / signature seront ajoutés
            // au moment où on construit le WireBlock pour le réseau.
            let mut wb = WireBlock {
                id: String::new(),
                parents: parents.clone(),
                payload_json: payload_json.clone(),
                nonce,
                network_id: String::new(),
                protocol_version: 0,
                signer_pk_hex: String::new(),
                signature_hex: String::new(),
                metadata: None,
            };

            let id = compute_id(&wb);

            if hash_meets_difficulty(&id, difficulty) {
                // on a trouvé un nonce valide
                wb.id = id.clone();
                return Block {
                    id,
                    parents: parents.clone(),
                    payload: payload.clone(),
                    nonce,
                    metadata: None,
                    signer_pk: None,
                    signature: None,
                };
            }

            nonce = nonce.wrapping_add(1);
        }
    }

    /// Insère un bloc **déjà persisté** dans le miroir RAM.
    pub fn commit_block(&mut self, b: Block) {
        for p in &b.parents {
            *self.children.entry(p.clone()).or_default() += 1;
        }
        let id = b.id.clone();
        self.blocks.insert(id.clone(), b);
        self.update_finality_after_insert(&id);
    }

    pub fn bump_children(&mut self, p: &str) {
        *self.children.entry(p.to_string()).or_default() += 1;
    }
}
