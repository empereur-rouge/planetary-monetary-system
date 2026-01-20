/**
 * Types TypeScript pour le SDK PMS.
 * 
 * Ces types correspondent aux structures de données de l'API REST PMS.
 */

// ═══════════════════════════════════════════════════════════════════════════
// UTXO Types
// ═══════════════════════════════════════════════════════════════════════════

/** Référence à un output (UTXO) */
export interface OutputRef {
    /** ID de la transaction */
    txid: string;
    /** Index de l'output dans la transaction */
    index: number;
}

/** Output de transaction (UTXO) */
export interface TxOutput {
    /** Adresse du destinataire */
    address: string;
    /** Montant en format décimal (ex: "10.50000000") */
    amount: string;
}

/** UTXO complet avec sa référence */
export interface Utxo extends TxOutput {
    /** Référence à cet UTXO */
    outpoint: OutputRef;
}

// ═══════════════════════════════════════════════════════════════════════════
// Transaction Types
// ═══════════════════════════════════════════════════════════════════════════

/** Transaction UTXO */
export interface TxUtxo {
    /** Inputs (UTXOs à dépenser) */
    inputs: OutputRef[];
    /** Outputs (nouveaux UTXOs) */
    outputs: TxOutput[];
    /** Frais de transaction */
    fee: string;
    /** Données optionnelles (memo) */
    data?: string;
}

/** Enveloppe de payload */
export type PayloadEnvelope =
    | { Plain: PlainPayload }
    | { Encrypted: EncryptedPayload };

/** Payload en clair */
export type PlainPayload =
    | { Mint: MintPayload }
    | { TxUtxo: TxUtxo }
    | { Milestone: MilestonePayload }
    | { Nft: NftAction }
    | { ConfigUpdate: ConfigUpdate };

/** Payload chiffré - compatible avec pms-types-payload/encrypted_payload.rs */
export interface EncryptedPayload {
    /** Schéma de chiffrement (toujours "x25519+aes256gcm") */
    scheme: string;
    /** Version de la clé (pour rotation future) */
    key_version: number;
    /** Métadonnée authentifiée (taille du payload) */
    aad: { len_hint: number };
    /** Hash SHA256 du plaintext pour vérification */
    commitment: string;
    /** Données chiffrées en base64 */
    ciphertext_b64: string;
    /** Liste des destinataires avec leurs clés enveloppées */
    recipients: KeyWrap[];
    /** Nonce AES-GCM en base64 (12 bytes) */
    nonce_b64: string;
}

/** Clé enveloppée pour un destinataire */
export interface KeyWrap {
    /** Identifiant opaque du destinataire (dérivé du secret partagé) */
    kid: string;
    /** Clé publique éphémère X25519 (hex) */
    ephem_pub: string;
    /** DEK chiffrée en base64 */
    wrapped_key_b64: string;
    /** Nonce pour le key wrap en base64 */
    kw_nonce_b64: string;
}

/** Mint de nouveaux tokens */
export interface MintPayload {
    recipient: string;
    amount: string;
    memo?: string;
}

/** Milestone (distribution des récompenses) */
export interface MilestonePayload {
    approved: string[];
    distribute_node_rewards: boolean;
}

/**
 * Métadonnées pour un NFT.
 * 
 * Ces champs sont tous optionnels pour offrir de la flexibilité,
 * mais au minimum `name` ou `uri` devraient être renseignés.
 */
export interface NftMetadata {
    /** Nom du NFT (ex: "Mon Artwork #1") */
    name?: string;
    /** Description du NFT */
    description?: string;
    /** URI vers les données/médias du NFT (ex: IPFS CID, URL) */
    uri?: string;
    /** Type de NFT (ex: "art", "collectible", "game_item") */
    nft_type?: string;
    /** Données supplémentaires libres */
    extra?: string;
}

/** Actions NFT */
export type NftAction =
    | { Mint: { token_id: string; owner: string; metadata: NftMetadata } }
    | { Transfer: { token_id: string; from: string; to: string } }
    | { Burn: { token_id: string; burner: string } }
    | { Use: { token_id: string; user: string; action_type: string } };

/** Mise à jour de configuration */
export type ConfigUpdate =
    | { SetFeeRate: { bps: number } }
    | { SetPlatformFee: { bps: number } }
    | { SetNodeFee: { bps: number } }
    | { SetMinPow: { bits: number } }
    | { SetMintEnabled: { enabled: boolean } };

// ═══════════════════════════════════════════════════════════════════════════
// Block Types
// ═══════════════════════════════════════════════════════════════════════════

/** Bloc du DAG */
export interface Block {
    /** ID unique du bloc (hash) */
    id: string;
    /** IDs des parents */
    parents: string[];
    /** Payload optionnel */
    payload?: PayloadEnvelope;
    /** Nonce PoW */
    nonce: number;
}

/** Bloc au format wire (pour soumission) */
export interface WireBlock {
    /** ID du bloc */
    id: string;
    /** Parents */
    parents: string[];
    /** Payload JSON sérialisé */
    payload_json?: string;
    /** Nonce PoW */
    nonce: number;
    /** ID du réseau */
    network_id: string;
    /** Version du protocole */
    protocol_version: number;
    /** Clé publique du signataire (hex) */
    signer_pk_hex: string;
    /** Signature (hex) */
    signature_hex: string;
}

// ═══════════════════════════════════════════════════════════════════════════
// API Response Types
// ═══════════════════════════════════════════════════════════════════════════

/** Informations sur le supply */
export interface SupplyInfo {
    /** Total en circulation */
    circulating: string;
    /** Nombre d'UTXOs */
    utxo_count: number;
}

/** Réponse de soumission de bloc */
export interface SubmitResponse {
    /** Statut: "inserted" ou "already_exists" */
    status: "inserted" | "already_exists";
    /** ID du bloc */
    block_id: string;
}

/** Attributs générés d'un Cube */
export interface CubeAttributes {
    /** Poids (1-100, hautes valeurs = rares) */
    weight: number;
    /** Taille (1-100, hautes valeurs = rares) */
    size: number;
    /** Densité (1-100, hautes valeurs = rares) */
    density: number;
}

/** Réponse de mintCube - inclut les données du cube généré */
export interface MintCubeResponse extends SubmitResponse {
    /** ID unique du cube (64 caractères hex) */
    token_id: string;
    /** Rareté du cube */
    rarity: "Unique" | "Legendary" | "Rare" | "Uncommon" | "Common" | "Basic";
    /** Roll aléatoire utilisé (0-9,999,999) */
    roll: number;
    /** Attributs générés */
    attributes: CubeAttributes;
}

/** Preview du remboursement pour un cube brûlé */
export interface RefundPreview {
    /** Montant du remboursement en PMS */
    amount: string;
    /** Adresse destinataire du remboursement */
    recipient: string;
}

/** Réponse de burnNft - inclut le refund preview si cube authentique */
export interface BurnNftResponse {
    /** Statut de l'opération ("burned", etc.) */
    status: string;
    /** ID du bloc créé dans le DAG */
    block_id: string;
    /** Token ID du NFT brûlé */
    token_id: string;
    /** Remboursement (si cube authentique avec signature Authority valide) */
    refund: RefundPreview | null;
}

/** Balance d'une adresse */
export interface BalanceInfo {
    /** Adresse */
    address: string;
    /** Balance totale */
    balance: string;
    /** Liste des UTXOs */
    utxos: Utxo[];
}

// ═══════════════════════════════════════════════════════════════════════════
// Node Registry Types
// ═══════════════════════════════════════════════════════════════════════════

/** Info sur un noeud du réseau */
export interface NodeInfo {
    /** Clé publique du noeud */
    node_pk: string;
    /** URL de l'API */
    api_url: string;
    /** Nombre de blocs produits */
    block_count: number;
    /** Timestamp dernière activité */
    last_seen: number;
}

/** Réponse de liste des noeuds */
export interface NodeListResponse {
    nodes: NodeInfo[];
}

// ═══════════════════════════════════════════════════════════════════════════
// Client Configuration
// ═══════════════════════════════════════════════════════════════════════════

/** Configuration du client PMS */
export interface PmsClientConfig {
    /** URL du nœud principal (ex: "https://node.pms.network") */
    nodeUrl: string;
    /** URLs des noeuds seeds (optionnel, pour racing et fallback) */
    seedNodes?: string[];
    /** ID du réseau (défaut: "pms-mainnet") */
    networkId?: string;
    /** Version du protocole (défaut: 1) */
    protocolVersion?: number;
    /** Timeout en ms (défaut: 30000) */
    timeout?: number;
    /** Activer le mode racing (envoie à tous les noeuds connus) (défaut: true) */
    enableRacing?: boolean;

}

/** Configuration par défaut */
export const DEFAULT_CONFIG: Required<Omit<PmsClientConfig, "nodeUrl" | "seedNodes">> & { seedNodes: string[] } = {
    networkId: "pms-mainnet",
    protocolVersion: 1,
    timeout: 30000,
    seedNodes: [],
    enableRacing: true,
};

/** Réponse de l'endpoint d'information du coordinateur */
export interface CoordinatorInfoResponse {
    /** Ce nœud est-il le Coordinateur ? */
    is_coordinator: boolean;
    /** Clé publique secp256k1 (hex) - pour vérifier les signatures */
    secp256k1_pubkey: string;
    /** Clé publique X25519 (hex) - pour le chiffrement */
    x25519_pubkey: string;
}
