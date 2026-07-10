//! Module de gestion des clés API pour l'authentification des clients SDK.
//!
//! # Architecture
//! - Les clés sont stockées dans un fichier JSON séparé (`api-keys.json`).
//! - Seul le **hash SHA-256** de chaque clé est stocké, jamais la clé en clair.
//! - Chaque clé a des **scopes** (permissions) qui déterminent les endpoints accessibles.
//! - Un `RwLock` protège le store pour les lectures concurrentes (middleware)
//!   et les écritures exclusives (CRUD admin).
//!
//! # Scopes disponibles
//! - `"*"` : accès total à tous les endpoints publics
//! - `"wallet"` : `/wallet/*`, `/v1/balance`, `/v1/tx/prepare`, `/v1/wallet/*`
//! - `"nft"` : `/v1/nft/*`, `/v1/wallet/{addr}/nfts`, `/v1/wallet/{addr}/utxos`
//! - `"dag"` : `/v1/dag/*`, `/v1/blocks/*`, `/v1/config`, `/submit/block`, `/blocks/stream`
//! - `"supply"` : `/v1/supply`, `/v1/fee_pool`
//! - `"tokens"` : `/v1/tokens`, `/v1/tokens/{id}`
//! - `"history"` : `/v1/history/*`, `/wallet/history`
//! - `"coordinator"` : `/v1/coordinator/*`
//! - Ou un path exact comme `"/v1/nft/mint"` pour granularité individuelle
//!
//! Voir chapitre 15 du Rust Book sur les Smart Pointers (Arc, RwLock)
//! pour comprendre pourquoi on a besoin de ces types ici.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

// ═══════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════

/// Une entrée dans le fichier `api-keys.json`.
///
/// On stocke le **hash** de la clé, jamais la clé en clair — comme pour un mot de passe.
/// La clé complète n'est montrée qu'une seule fois à la création.
///
/// # Ownership
/// Tous les champs sont des `String` (heap-allocated) car on sérialise/désérialise
/// depuis un fichier JSON. Voir chapitre 4.1 du Rust Book sur l'ownership des String.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyEntry {
    /// Identifiant unique de la clé (ex: "key_01")
    pub id: String,

    /// Hash SHA-256 de la clé secrète, en hexadécimal.
    /// On ne stocke JAMAIS la clé en clair pour des raisons de sécurité.
    pub key_hash: String,

    /// Label descriptif (ex: "Dashboard Production", "Clicker Game")
    pub label: String,

    /// Liste des scopes autorisés.
    /// - `["*"]` = accès total
    /// - `["wallet", "nft"]` = accès aux groupes wallet et nft
    /// - `["wallet", "/v1/nft/mint"]` = wallet complet + un endpoint spécifique
    pub scopes: Vec<String>,

    /// Clé active ou révoquée (soft-delete).
    /// Une clé désactivée retourne 403 même si le hash correspond.
    pub active: bool,

    /// Date de création (ISO 8601)
    pub created_at: String,
}

/// Format du fichier JSON sur disque.
///
/// On enveloppe le `Vec<ApiKeyEntry>` dans un objet pour pouvoir
/// ajouter des champs de metadata plus tard (version, etc.) sans
/// casser le format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeysFile {
    /// Liste des clés API
    pub keys: Vec<ApiKeyEntry>,
}

/// Réponse publique (sans le hash) pour l'endpoint `GET /admin/api-keys`.
/// On ne veut jamais exposer le hash dans l'API, même à l'admin.
#[derive(Debug, Serialize)]
pub struct ApiKeyPublicInfo {
    pub id: String,
    pub label: String,
    pub scopes: Vec<String>,
    pub active: bool,
    pub created_at: String,
}

/// Réponse unique lors de la création d'une clé.
/// C'est la SEULE fois où la clé en clair est retournée.
#[derive(Debug, Serialize)]
pub struct ApiKeyCreateResponse {
    pub id: String,
    pub key: String, // ← La clé en clair, montrée UNE SEULE FOIS
    pub label: String,
    pub scopes: Vec<String>,
    pub created_at: String,
}

/// Requête pour créer une nouvelle clé API.
#[derive(Debug, Deserialize)]
pub struct ApiKeyCreateRequest {
    pub label: String,
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
}

/// Par défaut, une clé a accès à tout.
fn default_scopes() -> Vec<String> {
    vec!["*".to_string()]
}

// ═══════════════════════════════════════════════════════════════════════
// ApiKeyStore — Gestion du fichier JSON + opérations CRUD
// ═══════════════════════════════════════════════════════════════════════

/// Store thread-safe pour les clés API.
///
/// # Thread Safety
/// - `Arc<RwLock<...>>` permet des lectures concurrentes (middleware) et
///   des écritures exclusives (CRUD admin).
/// - Voir chapitre 16.3 du Rust Book sur `Arc<Mutex<T>>` — ici on utilise
///   `RwLock` au lieu de `Mutex` car les lectures sont bien plus fréquentes.
pub type SharedApiKeyStore = Arc<RwLock<ApiKeyStore>>;

/// Store interne — ne pas utiliser directement, passer par `SharedApiKeyStore`.
#[derive(Debug)]
pub struct ApiKeyStore {
    /// Chemin vers le fichier JSON
    file_path: PathBuf,
    /// Cache en mémoire des clés
    keys: Vec<ApiKeyEntry>,
}

impl ApiKeyStore {
    /// Charge le store depuis un fichier JSON.
    ///
    /// Si le fichier n'existe pas, on crée un store vide.
    /// Si le fichier existe mais est mal formé, on retourne une erreur.
    ///
    /// # Errors
    /// Retourne une erreur si le fichier existe mais ne peut pas être parsé.
    pub fn load(file_path: impl AsRef<Path>) -> Result<Self, String> {
        let file_path = file_path.as_ref().to_path_buf();

        let keys = if file_path.exists() {
            // Lire et parser le fichier
            let content = std::fs::read_to_string(&file_path).map_err(|e| {
                format!("Cannot read API keys file '{}': {}", file_path.display(), e)
            })?;

            let file: ApiKeysFile = serde_json::from_str(&content).map_err(|e| {
                format!(
                    "Invalid JSON in API keys file '{}': {}",
                    file_path.display(),
                    e
                )
            })?;

            tracing::info!(
                "🔑 Loaded {} API key(s) from {}",
                file.keys.len(),
                file_path.display()
            );

            file.keys
        } else {
            tracing::info!(
                "🔑 API keys file not found at {}, starting with empty store",
                file_path.display()
            );
            Vec::new()
        };

        Ok(Self { file_path, keys })
    }

    /// Crée un store vide (pas de fichier, pas de vérification).
    /// Utilisé quand `api_keys_file` n'est pas configuré (mode dev).
    pub fn empty() -> Self {
        Self {
            file_path: PathBuf::new(),
            keys: Vec::new(),
        }
    }

    /// Retourne `true` si le store est vide (aucune clé configurée).
    /// En mode dev, on ne vérifie pas les clés API.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Sauvegarde l'état courant dans le fichier JSON.
    ///
    /// # Sécurité d'écriture
    /// On écrit dans un fichier temporaire puis on renomme (atomic rename).
    /// Ça évite de corrompre le fichier si le processus crash pendant l'écriture.
    fn save(&self) -> Result<(), String> {
        // Store en mémoire seul (pas de fichier configuré → tests, mode dev)
        if self.file_path.as_os_str().is_empty() {
            return Ok(());
        }

        let file = ApiKeysFile {
            keys: self.keys.clone(),
        };

        let content = serde_json::to_string_pretty(&file)
            .map_err(|e| format!("Failed to serialize API keys: {}", e))?;

        // Écriture atomique : tmp → rename
        // Ceci évite de corrompre le fichier si le processus est tué pendant l'écriture.
        let tmp_path = self.file_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &content)
            .map_err(|e| format!("Failed to write temp file: {}", e))?;
        std::fs::rename(&tmp_path, &self.file_path)
            .map_err(|e| format!("Failed to rename temp file: {}", e))?;

        Ok(())
    }

    // ─── CRUD ────────────────────────────────────────────────────────

    /// Crée une nouvelle clé API.
    ///
    /// 1. Génère 32 octets aléatoires → encode en hex → préfixe "pk_"
    /// 2. Calcule le SHA-256 du secret
    /// 3. Stocke l'entrée avec le hash (jamais la clé en clair)
    /// 4. Retourne la clé en clair une seule fois
    pub fn create_key(
        &mut self,
        label: String,
        scopes: Vec<String>,
    ) -> Result<ApiKeyCreateResponse, String> {
        // Génération d'un ID unique basé sur le compteur
        let next_id = self.keys.len() + 1;
        let id = format!("key_{:02}", next_id);

        // Génération de la clé secrète : 32 octets aléatoires → hex
        // `rand::random::<[u8; 32]>()` utilise le CSPRNG du système d'exploitation,
        // ce qui est cryptographiquement sûr. Voir docs.rs/rand pour les détails.
        let random_bytes: [u8; 32] = rand::random();
        let secret = format!("pk_{}", hex::encode(random_bytes));

        // Hash SHA-256 du secret — c'est ce qu'on stocke, jamais la clé en clair.
        // Voir chapitre "hashing" : on ne peut pas retrouver la clé à partir du hash.
        let key_hash = hash_api_key(&secret);

        let now = Utc::now().to_rfc3339();

        let entry = ApiKeyEntry {
            id: id.clone(),
            key_hash,
            label: label.clone(),
            scopes: scopes.clone(),
            active: true,
            created_at: now.clone(),
        };

        self.keys.push(entry);
        self.save()?;

        tracing::info!("🔑 Created API key '{}' (label: {})", id, label);

        Ok(ApiKeyCreateResponse {
            id,
            key: secret, // ← Montrée UNE SEULE FOIS
            label,
            scopes,
            created_at: now,
        })
    }

    /// Liste toutes les clés (sans les hashes, pour la sécurité).
    pub fn list_keys(&self) -> Vec<ApiKeyPublicInfo> {
        self.keys
            .iter()
            .map(|k| ApiKeyPublicInfo {
                id: k.id.clone(),
                label: k.label.clone(),
                scopes: k.scopes.clone(),
                active: k.active,
                created_at: k.created_at.clone(),
            })
            .collect()
    }

    /// Désactive une clé (soft-delete). La clé reste dans le fichier mais
    /// le middleware la rejettera.
    pub fn revoke_key(&mut self, key_id: &str) -> Result<(), String> {
        let entry = self
            .keys
            .iter_mut()
            .find(|k| k.id == key_id)
            .ok_or_else(|| format!("API key '{}' not found", key_id))?;

        if !entry.active {
            return Err(format!("API key '{}' is already revoked", key_id));
        }

        entry.active = false;
        self.save()?;

        tracing::info!("🔑 Revoked API key '{}'", key_id);
        Ok(())
    }

    // ─── Vérification (utilisé par le middleware) ────────────────────

    /// Vérifie une clé API reçue dans le header `X-API-Key`.
    ///
    /// Retourne `Some(&ApiKeyEntry)` si la clé est valide et active, `None` sinon.
    ///
    /// # Sécurité
    /// - On hash la clé reçue puis on compare les hashes en **temps constant**
    ///   (`subtle::ConstantTimeEq`) pour éviter les timing attacks.
    /// - On compare avec TOUTES les clés du store, même après avoir trouvé un match,
    ///   pour garder un temps d'exécution constant quel que soit le résultat.
    pub fn verify_key(&self, raw_key: &str) -> Option<&ApiKeyEntry> {
        // Hash la clé reçue une seule fois
        let received_hash = hash_api_key(raw_key);
        let received_bytes = received_hash.as_bytes();

        let mut matched_index: Option<usize> = None;

        // Itérer sur TOUTES les clés pour un temps constant
        // (même si on trouve un match au début, on continue)
        for (i, entry) in self.keys.iter().enumerate() {
            let stored_bytes = entry.key_hash.as_bytes();

            // Comparaison en temps constant (constant-time)
            // Voir helper.rs pour le pattern existant dans le projet.
            if stored_bytes.len() == received_bytes.len()
                && bool::from(stored_bytes.ct_eq(received_bytes))
            {
                matched_index = Some(i);
            }
        }

        // Vérifier que la clé est active
        matched_index.and_then(|i| {
            let entry = &self.keys[i];
            if entry.active { Some(entry) } else { None }
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Scope Resolution — Mappe un path HTTP vers son scope
// ═══════════════════════════════════════════════════════════════════════

/// Tous les scopes connus. Utilisé pour la validation à la création.
pub const KNOWN_SCOPES: &[&str] = &[
    "*",
    "wallet",
    "nft",
    "dag",
    "supply",
    "tokens",
    "sft",
    "history",
    "coordinator",
];

/// Détermine le scope d'une requête HTTP à partir de son path.
///
/// # Exemples
/// ```ignore
/// resolve_scope("/v1/balance")          → "wallet"
/// resolve_scope("/v1/nft/mint")         → "nft"
/// resolve_scope("/v1/dag/tips")         → "dag"
/// resolve_scope("/v1/supply")           → "supply"
/// resolve_scope("/v1/tokens")           → "tokens"
/// resolve_scope("/v1/history/plain")    → "history"
/// resolve_scope("/v1/coordinator/info") → "coordinator"
/// ```
///
/// # Règles de matching
/// On utilise des préfixes de path pour déterminer le scope.
/// L'ordre des `if` est important : du plus spécifique au plus général.
pub fn resolve_scope(path: &str) -> &'static str {
    // ── Wallet ──
    if path.starts_with("/wallet/")
        || path == "/v1/balance"
        || path.starts_with("/v1/tx/")
        || path.starts_with("/v1/wallet/create")
        || path.starts_with("/v1/wallet/send")
    {
        return "wallet";
    }

    // ── NFT ──
    // Note: "/v1/wallet/{addr}/nfts" et "/v1/wallet/{addr}/utxos" sont dans le scope NFT
    // car ils servent à lister les NFTs d'un wallet, pas à gérer le wallet lui-même.
    if path.starts_with("/v1/nft/") || path.starts_with("/v1/wallet/") {
        return "nft";
    }

    // ── DAG ──
    if path.starts_with("/v1/dag/")
        || path.starts_with("/v1/blocks/")
        || path == "/v1/config"
        || path.starts_with("/submit/")
        || path.starts_with("/blocks/")
    {
        return "dag";
    }

    // ── Supply ──
    if path == "/v1/supply" || path == "/v1/fee_pool" {
        return "supply";
    }

    // ── Tokens ── (inclut la création/mint custodiale `/v1/tokens/create`,
    // `/v1/tokens/mint`, `/v1/tokens/mint/prepare` — protocole 2.8)
    if path.starts_with("/v1/tokens") {
        return "tokens";
    }

    // ── SFT (semi-fongibles) ── création/mint custodiale (protocole 2.8) :
    // `/v1/sft/classes`, `/v1/sft/mint`, `/v1/sft/mint/prepare`. (Les GET
    // catalogue `/v1/sft/...` sont publics — non gated, resolve_scope non consulté.)
    if path.starts_with("/v1/sft/") {
        return "sft";
    }

    // ── History ──
    if path.starts_with("/v1/history/") {
        return "history";
    }

    // ── Coordinator ──
    if path.starts_with("/v1/coordinator/") {
        return "coordinator";
    }

    // Fallback : scope inconnu — le middleware refusera si la clé n'a pas "*"
    "unknown"
}

/// Vérifie si une clé API a la permission d'accéder à un path donné.
///
/// Deux modes de vérification :
/// 1. **Par scope** : la clé a le scope du groupe (ex: `"wallet"` → accès à tout `/wallet/*`)
/// 2. **Par path exact** : la clé a le path exact dans ses scopes (ex: `"/v1/nft/mint"`)
///
/// Le scope `"*"` (wildcard) donne accès à tout.
pub fn has_permission(entry: &ApiKeyEntry, request_path: &str) -> bool {
    let scope = resolve_scope(request_path);

    for s in &entry.scopes {
        // Wildcard = accès total
        if s == "*" {
            return true;
        }
        // Match par scope (groupe)
        if s == scope {
            return true;
        }
        // Match par path exact (granularité individuelle)
        if request_path.starts_with(s.as_str()) {
            return true;
        }
    }

    false
}

// ═══════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════

/// Calcule le SHA-256 d'une clé API et retourne le hash en hexadécimal.
///
/// On utilise `sha2::Sha256` (crate `sha2`) qui est la même implémentation
/// que celle utilisée dans le reste du projet (blocs, transactions, etc.)
fn hash_api_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hex::encode(hasher.finalize())
}

/// Crée un `SharedApiKeyStore` prêt à l'emploi.
///
/// - Si `file_path` est `Some(...)`, charge depuis le fichier.
/// - Si `None`, retourne un store vide (pas de vérification, mode dev).
pub fn create_api_key_store(file_path: Option<&str>) -> Result<SharedApiKeyStore, String> {
    let store = match file_path {
        Some(path) => ApiKeyStore::load(path)?,
        None => {
            tracing::info!(
                "🔑 No api_keys_file configured — API key verification disabled (dev mode)"
            );
            ApiKeyStore::empty()
        }
    };
    Ok(Arc::new(RwLock::new(store)))
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── hash_api_key ──

    #[test]
    fn hash_is_deterministic() {
        let h1 = hash_api_key("pk_test_hello");
        let h2 = hash_api_key("pk_test_hello");
        assert_eq!(h1, h2, "Same input must produce same hash");
    }

    #[test]
    fn different_keys_produce_different_hashes() {
        let h1 = hash_api_key("pk_test_aaa");
        let h2 = hash_api_key("pk_test_bbb");
        assert_ne!(h1, h2, "Different inputs must produce different hashes");
    }

    // ── resolve_scope ──

    #[test]
    fn scope_wallet_routes() {
        assert_eq!(resolve_scope("/wallet/tx/send"), "wallet");
        assert_eq!(resolve_scope("/wallet/balance"), "wallet");
        assert_eq!(resolve_scope("/wallet/history"), "wallet");
        assert_eq!(resolve_scope("/v1/balance"), "wallet");
        assert_eq!(resolve_scope("/v1/tx/prepare"), "wallet");
        assert_eq!(resolve_scope("/v1/wallet/create"), "wallet");
        assert_eq!(resolve_scope("/v1/wallet/send-simple"), "wallet");
    }

    #[test]
    fn scope_nft_routes() {
        assert_eq!(resolve_scope("/v1/nft/mint"), "nft");
        assert_eq!(resolve_scope("/v1/nft/burn"), "nft");
        assert_eq!(resolve_scope("/v1/nft/burn-simple"), "nft");
        assert_eq!(resolve_scope("/v1/nft/abc123"), "nft");
        assert_eq!(resolve_scope("/v1/wallet/04abcdef/nfts"), "nft");
        assert_eq!(resolve_scope("/v1/wallet/04abcdef/utxos"), "nft");
    }

    #[test]
    fn scope_dag_routes() {
        assert_eq!(resolve_scope("/v1/dag/tips"), "dag");
        assert_eq!(resolve_scope("/v1/blocks/abc123"), "dag");
        assert_eq!(resolve_scope("/v1/config"), "dag");
        assert_eq!(resolve_scope("/submit/block"), "dag");
        assert_eq!(resolve_scope("/blocks/stream"), "dag");
    }

    #[test]
    fn scope_supply_routes() {
        assert_eq!(resolve_scope("/v1/supply"), "supply");
        assert_eq!(resolve_scope("/v1/fee_pool"), "supply");
    }

    #[test]
    fn scope_tokens_routes() {
        assert_eq!(resolve_scope("/v1/tokens"), "tokens");
        assert_eq!(resolve_scope("/v1/tokens/abc123"), "tokens");
    }

    #[test]
    fn scope_history_routes() {
        assert_eq!(resolve_scope("/v1/history/encrypted"), "history");
        assert_eq!(resolve_scope("/v1/history/plain"), "history");
    }

    #[test]
    fn scope_coordinator_routes() {
        assert_eq!(resolve_scope("/v1/coordinator/info"), "coordinator");
    }

    #[test]
    fn scope_unknown_fallback() {
        assert_eq!(resolve_scope("/some/random/path"), "unknown");
    }

    // ── has_permission ──

    fn make_entry(scopes: Vec<&str>) -> ApiKeyEntry {
        ApiKeyEntry {
            id: "test".into(),
            key_hash: "abc".into(),
            label: "test".into(),
            scopes: scopes.into_iter().map(String::from).collect(),
            active: true,
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn wildcard_allows_everything() {
        let entry = make_entry(vec!["*"]);
        assert!(has_permission(&entry, "/v1/balance"));
        assert!(has_permission(&entry, "/v1/nft/mint"));
        assert!(has_permission(&entry, "/v1/supply"));
        assert!(has_permission(&entry, "/some/random/path"));
    }

    #[test]
    fn scope_grants_group_access() {
        let entry = make_entry(vec!["wallet", "nft"]);
        assert!(has_permission(&entry, "/v1/balance")); // wallet
        assert!(has_permission(&entry, "/v1/nft/mint")); // nft
        assert!(!has_permission(&entry, "/v1/supply")); // supply — not allowed
        assert!(!has_permission(&entry, "/v1/dag/tips")); // dag — not allowed
    }

    #[test]
    fn exact_path_grants_single_endpoint() {
        let entry = make_entry(vec!["wallet", "/v1/nft/mint"]);
        assert!(has_permission(&entry, "/v1/balance")); // wallet scope
        assert!(has_permission(&entry, "/v1/nft/mint")); // exact path
        assert!(!has_permission(&entry, "/v1/nft/burn")); // not in scopes
    }

    #[test]
    fn empty_scopes_deny_everything() {
        let entry = make_entry(vec![]);
        assert!(!has_permission(&entry, "/v1/balance"));
        assert!(!has_permission(&entry, "/v1/nft/mint"));
    }

    // ── ApiKeyStore CRUD ──

    #[test]
    fn create_and_verify_key() {
        let mut store = ApiKeyStore::empty();

        let resp = store
            .create_key("Test Key".into(), vec!["wallet".into()])
            .expect("create should succeed on empty store");

        // La clé retournée doit commencer par "pk_"
        assert!(resp.key.starts_with("pk_"), "Key must start with pk_");

        // On doit pouvoir vérifier la clé
        let entry = store.verify_key(&resp.key);
        assert!(entry.is_some(), "Key must be verifiable");

        let entry = entry.unwrap();
        assert_eq!(entry.id, resp.id);
        assert_eq!(entry.label, "Test Key");
        assert!(entry.active);
    }

    #[test]
    fn wrong_key_fails_verification() {
        let mut store = ApiKeyStore::empty();
        store
            .create_key("Test".into(), vec!["*".into()])
            .expect("create should succeed");

        let result = store.verify_key("pk_wrong_key_123");
        assert!(result.is_none(), "Wrong key must not verify");
    }

    #[test]
    fn revoked_key_fails_verification() {
        let mut store = ApiKeyStore::empty();
        let resp = store
            .create_key("Test".into(), vec!["*".into()])
            .expect("create should succeed");

        // Vérification avant révocation
        assert!(store.verify_key(&resp.key).is_some());

        // Révoquer
        store.revoke_key(&resp.id).expect("revoke should succeed");

        // Vérification après révocation
        assert!(
            store.verify_key(&resp.key).is_none(),
            "Revoked key must not verify"
        );
    }

    #[test]
    fn list_keys_omits_hashes() {
        let mut store = ApiKeyStore::empty();
        store
            .create_key("Key A".into(), vec!["wallet".into()])
            .expect("create should succeed");
        store
            .create_key("Key B".into(), vec!["*".into()])
            .expect("create should succeed");

        let list = store.list_keys();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].label, "Key A");
        assert_eq!(list[1].label, "Key B");
    }

    // ── File persistence ──

    #[test]
    fn load_save_roundtrip() {
        let dir = std::env::temp_dir().join("pms_api_keys_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test-keys.json");

        // Nettoyer
        let _ = std::fs::remove_file(&path);

        // Créer et sauvegarder
        let mut store = ApiKeyStore::load(&path).expect("load empty should work");
        let resp = store
            .create_key("Persistent".into(), vec!["nft".into(), "wallet".into()])
            .expect("create should succeed");

        // Recharger depuis le fichier
        let store2 = ApiKeyStore::load(&path).expect("reload should work");
        assert_eq!(store2.keys.len(), 1);

        // Vérifier la clé dans le store rechargé
        let entry = store2.verify_key(&resp.key);
        assert!(entry.is_some(), "Key must survive reload");
        assert_eq!(entry.unwrap().label, "Persistent");

        // Nettoyer
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
