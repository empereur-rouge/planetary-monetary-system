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

    /// Enregistre ou met à jour un nœud
    pub fn register(&mut self, node_pk: String, api_url: String, wallet_address: Option<String>) {
        let entry = self.nodes.entry(node_pk.clone()).or_insert(NodeInfo {
            node_pk: node_pk.clone(),
            api_url: api_url.clone(),
            wallet_address: wallet_address.clone(),
            block_count: 0,
            last_seen: Some(Instant::now()),
        });
        entry.api_url = api_url;
        // Met à jour l'adresse si fournie, sinon garde l'ancienne
        if wallet_address.is_some() {
            entry.wallet_address = wallet_address;
        }
        entry.last_seen = Some(Instant::now());
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

/// Requête pour POST /v1/register
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterNodeRequest {
    pub node_pk: String,
    pub api_url: String,
    pub wallet_address: Option<String>,
}

/// Réponse pour GET /v1/nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodesListResponse {
    pub nodes: Vec<NodeInfo>,
}
