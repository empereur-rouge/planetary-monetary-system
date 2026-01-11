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

/** Payload chiffré (pour l'instant non supporté dans le SDK) */
export interface EncryptedPayload {
    ciphertext: string;
    nonce: string;
    recipient_pk: string;
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

/** Actions NFT */
export type NftAction =
    | { Mint: { token_id: string; owner: string; metadata: Record<string, unknown> } }
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
