// ============================================================================
// Node Registry - Dynamic node discovery for distributed transaction processing
// ============================================================================
//
// Each node registers itself on startup via POST /v1/register
// Clients can query GET /v1/nodes to get list of available nodes
// Nodes are removed if not seen for TTL_SECONDS
//
// Voir chapitre 16 du Rust Book pour les smart pointers (Arc, RwLock)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Durée avant qu'un nœud soit considéré comme inactif (24 heures pour éviter les timeouts en dev)
const NODE_TTL_SECONDS: u64 = 86400;

/// Nombre maximum de nœuds distincts dans le registre. Garde anti-DoS pour le
/// chemin d'inscription self-authentifiée (`/v1/register` par signature) : sans
/// cap, un attaquant pourrait générer des paires de clés et spammer des entrées
/// (chaque entrée valide car auto-signée) → croissance mémoire non bornée avant
/// l'expiration TTL. Les mises à jour d'un nœud DÉJÀ présent restent permises.
pub const MAX_REGISTERED_NODES: usize = 10_000;

/// Information sur un nœud enregistré
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    /// Clé publique du nœud (identifiant unique)
    pub node_pk: String,
    /// URL de l'API du nœud (ex: "https://node1.example.com:8080")
    pub api_url: String,
    /// Adresse du wallet pour recevoir les rewards
    pub wallet_address: Option<String>,
    /// Nombre de blocs créés par ce nœud (pour la distribution des rewards)
    #[serde(default)]
    pub block_count: u64,
    /// Dernier heartbeat (non sérialisé)
    #[serde(skip)]
    pub last_seen: Option<Instant>,
    /// Timestamp (ms) de la dernière inscription AUTHENTIFIÉE par signature de
    /// `node_pk` (anti-rejeu monotone : une nouvelle inscription signée doit
    /// porter un `ts_ms` strictement supérieur). Non exposé. `0` = jamais
    /// inscrit par signature (ex: posé par l'admin).
    #[serde(skip)]
    pub last_auth_ts: i64,
}

/// Registre des nœuds actifs
#[derive(Debug, Default)]
pub struct NodeRegistry {
    /// Map: node_pk -> NodeInfo
    nodes: HashMap<String, NodeInfo>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    /// Enregistre ou met à jour un nœud.
    ///
    /// Piggybacks a stale-node cleanup to prevent unbounded HashMap growth.
    /// **Chemin admin uniquement** (autorisé par le token opérateur) — ne touche
    /// pas `last_auth_ts` (réservé au chemin signature). Pour l'inscription
    /// self-authentifiée d'un pair, utiliser [`try_register_authenticated`].
    pub fn register(&mut self, node_pk: String, api_url: String, wallet_address: Option<String>) {
        self.cleanup_stale();
        let entry = self.nodes.entry(node_pk.clone()).or_insert(NodeInfo {
            node_pk: node_pk.clone(),
            api_url: api_url.clone(),
            wallet_address: wallet_address.clone(),
            block_count: 0,
            last_seen: Some(Instant::now()),
            last_auth_ts: 0,
        });
        entry.api_url = api_url;
        // Met à jour l'adresse si fournie, sinon garde l'ancienne
        if wallet_address.is_some() {
            entry.wallet_address = wallet_address;
        }
        entry.last_seen = Some(Instant::now());
    }

    /// Inscription/mise à jour **self-authentifiée** par un pair (signature de
    /// `node_pk` déjà vérifiée par l'appelant). Applique deux gardes ATOMIQUES
    /// (sous `&mut self`) :
    ///
    /// - **Anti-rejeu monotone** : `ts_ms` doit être strictement supérieur au
    ///   `last_auth_ts` de l'entrée existante — une signature capturée ne peut
    ///   pas être rejouée pour revenir à un `api_url`/`wallet_address` antérieur.
    /// - **Cap anti-DoS** : une NOUVELLE clé n'est acceptée que si le registre
    ///   n'a pas atteint `MAX_REGISTERED_NODES` (les mises à jour d'un nœud
    ///   existant restent toujours permises).
    ///
    /// `Err(reason)` sur violation (l'appelant renvoie 401/429).
    pub fn try_register_authenticated(
        &mut self,
        node_pk: String,
        api_url: String,
        wallet_address: Option<String>,
        ts_ms: i64,
    ) -> Result<(), String> {
        self.cleanup_stale();
        match self.nodes.get(&node_pk) {
            Some(existing) => {
                if ts_ms <= existing.last_auth_ts {
                    return Err(format!(
                        "stale/replayed registration ts {ts_ms} <= last {}",
                        existing.last_auth_ts
                    ));
                }
            }
            None => {
                if self.nodes.len() >= MAX_REGISTERED_NODES {
                    return Err(format!(
                        "node registry full ({MAX_REGISTERED_NODES}) — new registrations rejected"
                    ));
                }
            }
        }
        self.register(node_pk.clone(), api_url, wallet_address);
        if let Some(n) = self.nodes.get_mut(&node_pk) {
            n.last_auth_ts = ts_ms;
        }
        Ok(())
    }

    /// Incrémente le compteur de blocs pour un nœud
    pub fn increment_block_count(&mut self, node_pk: &str) {
        if let Some(node) = self.nodes.get_mut(node_pk) {
            node.block_count += 1;
        }
    }

    /// Retourne la liste des nœuds actifs (TTL pas expiré)
    /// La liste est mélangée aléatoirement pour la distribution
    pub fn get_active_nodes(&self) -> Vec<NodeInfo> {
        use rand::seq::SliceRandom;

        let now = Instant::now();
        let ttl = Duration::from_secs(NODE_TTL_SECONDS);

        let mut active: Vec<NodeInfo> = self
            .nodes
            .values()
            .filter(|n| {
                n.last_seen
                    .map(|t| now.duration_since(t) < ttl)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();

        // Mélange aléatoire pour la distribution équitable
        let mut rng = rand::rng();
        active.shuffle(&mut rng);

        active
    }

    /// Retourne les compteurs de blocs pour la distribution des rewards
    pub fn get_block_counts(&self) -> HashMap<String, u64> {
        self.nodes
            .iter()
            .map(|(pk, info)| (pk.clone(), info.block_count))
            .collect()
    }

    /// Remet les compteurs de blocs à zéro (après distribution)
    pub fn reset_block_counts(&mut self) {
        for node in self.nodes.values_mut() {
            node.block_count = 0;
        }
    }

    /// Supprime les nœuds inactifs
    pub fn cleanup_stale(&mut self) {
        let now = Instant::now();
        let ttl = Duration::from_secs(NODE_TTL_SECONDS);

        self.nodes.retain(|_, n| {
            n.last_seen
                .map(|t| now.duration_since(t) < ttl)
                .unwrap_or(false)
        });
    }
}

/// Type thread-safe pour le registre
pub type SharedNodeRegistry = Arc<RwLock<NodeRegistry>>;

/// Crée un nouveau registre partagé
pub fn create_registry() -> SharedNodeRegistry {
    Arc::new(RwLock::new(NodeRegistry::new()))
}

// ============================================================================
// Request/Response types for API endpoints
// ============================================================================

/// Requête pour POST /v1/register (et /v1/heartbeat).
///
/// Autorisation : SOIT le token admin (chemin opérateur), SOIT une preuve de
/// possession de `node_pk` — signature détachée de `node_pk` sur
/// `node_register_signing_message(network_id, node_pk, api_url, wallet_address,
/// ts_ms)`. Sur le chemin signature, `ts_ms` et `signature_b64` sont requis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterNodeRequest {
    pub node_pk: String,
    pub api_url: String,
    #[serde(default)]
    pub wallet_address: Option<String>,
    /// Timestamp (ms epoch) du message signé — chemin signature. Vérifié pour la
    /// fraîcheur (fenêtre) et la monotonie (anti-rejeu).
    #[serde(default)]
    pub ts_ms: Option<i64>,
    /// Signature détachée (DER base64) de `node_pk` sur le message canonique.
    #[serde(default)]
    pub signature_b64: Option<String>,
}

/// Réponse pour GET /v1/nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodesListResponse {
    pub nodes: Vec<NodeInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(node_pk: &str, reg: &NodeRegistry) -> Option<String> {
        reg.nodes.get(node_pk).map(|n| n.api_url.clone())
    }

    #[test]
    fn authenticated_register_is_monotonic_anti_replay() {
        let mut reg = NodeRegistry::new();
        // First signed registration (ts=1000) accepted.
        assert!(reg.try_register_authenticated("pk1".into(), "url_a".into(), None, 1000).is_ok());
        assert_eq!(addr("pk1", &reg).as_deref(), Some("url_a"));

        // Replay of the SAME ts → rejected; does NOT overwrite.
        assert!(reg.try_register_authenticated("pk1".into(), "url_evil".into(), None, 1000).is_err());
        assert_eq!(addr("pk1", &reg).as_deref(), Some("url_a"), "replay must not overwrite");

        // OLDER ts → rejected (rollback prevented).
        assert!(reg.try_register_authenticated("pk1".into(), "url_evil".into(), None, 500).is_err());
        assert_eq!(addr("pk1", &reg).as_deref(), Some("url_a"));

        // Strictly NEWER ts → accepted, updates the entry.
        assert!(reg.try_register_authenticated("pk1".into(), "url_b".into(), None, 2000).is_ok());
        assert_eq!(addr("pk1", &reg).as_deref(), Some("url_b"));
    }

    #[test]
    fn authenticated_register_caps_new_nodes_but_allows_updates() {
        let mut reg = NodeRegistry::new();
        for i in 0..MAX_REGISTERED_NODES {
            assert!(
                reg.try_register_authenticated(format!("pk{i}"), "u".into(), None, 1).is_ok(),
                "fill {i} must succeed"
            );
        }
        // A NEW pk beyond the cap → rejected as "full".
        let over = reg.try_register_authenticated("pk-over".into(), "u".into(), None, 1);
        assert!(over.is_err(), "new node beyond cap must be rejected");
        assert!(over.unwrap_err().contains("full"), "reason must mention 'full'");

        // An UPDATE to an EXISTING node (strictly newer ts) is still allowed.
        assert!(
            reg.try_register_authenticated("pk0".into(), "u2".into(), None, 2).is_ok(),
            "update to existing node must be allowed even at cap"
        );
    }
}
