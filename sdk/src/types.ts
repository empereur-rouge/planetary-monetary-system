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
    /** Asset ID (undefined/null = PMS natif, "edenite" = token custom) */
    asset_id?: string;
}

/** UTXO complet avec sa référence */
export interface Utxo extends TxOutput {
    /** Référence à cet UTXO */
    outpoint: OutputRef;
}

// ═══════════════════════════════════════════════════════════════════════════
// Transaction Types
// ═══════════════════════════════════════════════════════════════════════════

/** Input de transaction (wrapper autour de OutputRef) */
export interface TxInput {
    /** Référence à l'UTXO dépensé */
    out: OutputRef;
}

/** Unlock (signature pour un input) */
export interface Unlock {
    /** Clé publique hex */
    pubkey_hex: string;
    /** Signature base64 */
    signature_b64: string;
}

/** Transaction UTXO */
export interface TxUtxo {
    /** Inputs (UTXOs à dépenser) */
    inputs: TxInput[];
    /** Outputs (nouveaux UTXOs) */
    outputs: TxOutput[];
    /** Frais de transaction */
    fee: string;
    /** Unlocks (signatures pour chaque input) */
    unlocks: Unlock[];
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
    | { ConfigUpdate: ConfigUpdate }
    | { Reward: RewardPayload }
    | { EncryptedReward: EncryptedRewardPayload }
    | { TokenCreate: TokenMetadata };

/** Payload de récompense (Mining/Fees) */
export interface RewardPayload {
    fee_outputs: TaggedOutput[];
    reward_outputs: TxOutput[];
    burned: string;
    tx_block_id: string;
}

/** Output avec tag (pour fees) */
export interface TaggedOutput extends TxOutput {
    tag: string; // e.g. "NodeFee", "BurnRefund"
}

/** Payload de récompense chiffrée */
export interface EncryptedRewardPayload {
    encrypted_outputs: EncryptedRewardOutput[];
    burned: string;
    tx_block_id: string;
}

/** Output de récompensation chiffré individuellement */
export interface EncryptedRewardOutput {
    encrypted: EncryptedPayload;
}

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
    | { BatchBurn: { token_ids: string[]; burner: string } }
    | { Use: { token_id: string; user: string; action_type: string } };

/** Métadonnées d'un token enregistré dans le DAG */
export interface TokenMetadata {
    /** Identifiant unique du token (ex: "edenite") */
    asset_id: string;
    /** Symbole court (ex: "EDEN") */
    symbol: string;
    /** Nom complet (ex: "Edenite Token") */
    name: string;
    /** Nombre de décimales (ex: 8) */
    decimals: number;
    /** Supply maximum (undefined = illimité) */
    max_supply?: string;
    /** Adresse du créateur */
    creator: string;
    /** Clé publique autorisée à mint ce token */
    mint_authority: string;
}

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
    /** Token ID du NFT brûlé (ou "batch") */
    token_id: string;
    /** Liste des token IDs brûlés (pour batch burn) */
    token_ids?: string[];
    /** Remboursement (si cube authentique avec signature Authority valide) */
    refund: RefundPreview | null;
}

/** Requête pour préparer un transfert avec re-chiffrement */
export interface PrepareTransferRequest {
    token_id: string;
    to_address: string;
    from_address: string;
    new_owner_x25519_pubkey: string;
}

/** Réponse de préparation de transfert */
export interface PrepareTransferResponse {
    action: NftAction;
}

// ═══════════════════════════════════════════════════════════════════════════
// TX Prepare Types
// ═══════════════════════════════════════════════════════════════════════════

/** Requête pour préparer une transaction de transfert */
export interface PrepareTxRequest {
    /** Adresse Bech32 de l'expéditeur */
    from: string;
    /** Adresse Bech32 du destinataire */
    to: string;
    /** Montant à envoyer (décimal, ex: "100.5") */
    amount: string;
    /** Asset ID (undefined = PMS natif, "edenite" = token custom) */
    asset_id?: string;
}

/** Détail d'un UTXO utilisé comme input */
export interface UtxoDetail {
    txid: string;
    index: number;
    amount: string;
}

/** Réponse de /v1/tx/prepare - transaction non-signée */
export interface PrepareTxResponse {
    /** Transaction non-signée (unlocks vide) */
    unsigned_tx: TxUtxo;
    /** Hash SHA256 du message à signer (hex) */
    tx_hash: string;
    /** Frais calculés */
    fee: string;
    /** Détail des UTXOs sélectionnés */
    inputs_detail: UtxoDetail[];
}

/** Réponse de getNft - informations complètes d'un NFT */
export interface NftResponse {
    /** Token ID du NFT */
    token_id: string;
    /** Propriétaire actuel (null si le token n'existe pas) */
    owner: string | null;
    /** ID du bloc contenant les métadonnées chiffrées (pour récupération + déchiffrement client) */
    mint_block_id: string | null;
    /** Existe-t-il sur le DAG ? */
    exists: boolean;
    /** @deprecated Les métadonnées sont maintenant chiffrées dans le bloc source */
    metadata?: NftMetadata;
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

// ═══════════════════════════════════════════════════════════════════════════
// History Types
// ═══════════════════════════════════════════════════════════════════════════

/** Élément d'historique de wallet */
export interface HistoryItem {
    block_id: string;
    ts_ms: number;
    payload_type: string; // "Mint", "TxUtxo", "Reward", "EncryptedReward"
    payload: any;         // Typé dynamiquement selon payload_type
}

/** Réponse de l'historique du wallet */
export interface WalletHistoryResp {
    address: string;
    items: HistoryItem[];
    count: number;
}

// ═══════════════════════════════════════════════════════════════════════════
// Config Types
// ═══════════════════════════════════════════════════════════════════════════

export interface RuntimeConfig {
    fee_rate_bps: number;
    base_fee: string;
    platform_fee_bps: number;
    node_fee_bps: number;
    min_pow_bits: number;
    max_mint_per_block: number;
    mint_enabled: boolean;
    updated_at_block: string;
    updated_at_timestamp: number;
}

/** Config publique retournée par /v1/config */
export interface NodePublicConfig extends RuntimeConfig {
    fee_recipient: string;
}
