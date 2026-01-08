/**
 * PmsWallet - Wallet pour le réseau PMS.
 *
 * Supporte:
 * - Génération de wallet avec mnémonique 24 mots (BIP39)
 * - Import depuis clé privée
 * - Signature de messages
 * - Export de la phrase mnémonique
 *
 * @example
 * ```typescript
 * // Générer un nouveau wallet
 * const wallet = PmsWallet.generate();
 * console.log(wallet.mnemonic); // 24 mots
 *
 * // Restaurer depuis mnemonic
 * const wallet2 = PmsWallet.fromMnemonic("word1 word2 ...");
 *
 * // Signer un message
 * const sig = await wallet.sign(messageBytes);
 * ```
 */
/**
 * Wallet PMS avec gestion des clés cryptographiques.
 */
declare class PmsWallet {
    /** Clé privée (32 bytes) */
    private readonly _privateKey;
    /** Clé publique non compressée (65 bytes: 04 + x + y) */
    private readonly _publicKey;
    /** Phrase mnémonique (24 mots) si générée/importée */
    private readonly _mnemonic?;
    /**
     * Constructeur privé - utiliser les méthodes statiques.
     */
    private constructor();
    /**
     * Génère un nouveau wallet avec une phrase de 24 mots.
     */
    static generate(): PmsWallet;
    /**
     * Crée un wallet à partir d'une phrase mnémonique (12, 15, 18, 21 ou 24 mots).
     * @throws Error si la phrase est invalide
     */
    static fromMnemonic(mnemonic: string): PmsWallet;
    /**
     * Crée un wallet à partir d'une clé privée hexadécimale.
     */
    static fromPrivateKey(privateKeyHex: string): PmsWallet;
    /**
     * Crée un wallet à partir d'une seed (32 bytes).
     */
    static fromSeed(seed: Uint8Array): PmsWallet;
    /**
     * Adresse du wallet (clé publique hex).
     * Format: "04" + 64 bytes hex = 130 caractères
     */
    get address(): string;
    /**
     * Clé publique en bytes.
     */
    get publicKey(): Uint8Array;
    /**
     * Clé publique en hex.
     */
    get publicKeyHex(): string;
    /**
     * Phrase mnémonique (si disponible).
     * @returns undefined si le wallet a été créé depuis une clé privée
     */
    get mnemonic(): string | undefined;
    /**
     * Signe un message avec la clé privée.
     * @param message - Message à signer (sera hashé avec SHA256)
     * @returns Signature DER encodée en hex
     */
    sign(message: Uint8Array): string;
    /**
     * Signe un message déjà hashé.
     * @param hash - Hash 32 bytes du message
     * @returns Signature DER encodée en hex
     */
    signHash(hash: Uint8Array): string;
    /**
     * Exporte la clé privée en hex.
     * ⚠️ À utiliser avec précaution !
     */
    exportPrivateKey(): string;
    /**
     * Vérifie une signature.
     * @param message - Message original
     * @param signature - Signature DER hex
     * @param publicKeyHex - Clé publique hex du signataire
     */
    static verify(message: Uint8Array, signature: string, publicKeyHex: string): boolean;
}
/**
 * Vérifie si une phrase mnémonique est valide.
 */
declare function isValidMnemonic(mnemonic: string): boolean;

/**
 * Types TypeScript pour le SDK PMS.
 *
 * Ces types correspondent aux structures de données de l'API REST PMS.
 */
/** Référence à un output (UTXO) */
interface OutputRef {
    /** ID de la transaction */
    txid: string;
    /** Index de l'output dans la transaction */
    index: number;
}
/** Output de transaction (UTXO) */
interface TxOutput {
    /** Adresse du destinataire */
    address: string;
    /** Montant en format décimal (ex: "10.50000000") */
    amount: string;
}
/** UTXO complet avec sa référence */
interface Utxo extends TxOutput {
    /** Référence à cet UTXO */
    outpoint: OutputRef;
}
/** Transaction UTXO */
interface TxUtxo {
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
type PayloadEnvelope = {
    Plain: PlainPayload;
} | {
    Encrypted: EncryptedPayload;
};
/** Payload en clair */
type PlainPayload = {
    Mint: MintPayload;
} | {
    TxUtxo: TxUtxo;
} | {
    Milestone: MilestonePayload;
} | {
    Nft: NftAction;
} | {
    ConfigUpdate: ConfigUpdate;
};
/** Payload chiffré (pour l'instant non supporté dans le SDK) */
interface EncryptedPayload {
    ciphertext: string;
    nonce: string;
    recipient_pk: string;
}
/** Mint de nouveaux tokens */
interface MintPayload {
    recipient: string;
    amount: string;
    memo?: string;
}
/** Milestone (distribution des récompenses) */
interface MilestonePayload {
    approved: string[];
    distribute_node_rewards: boolean;
}
/** Actions NFT */
type NftAction = {
    Mint: {
        token_id: string;
        owner: string;
        metadata: Record<string, unknown>;
    };
} | {
    Transfer: {
        token_id: string;
        from: string;
        to: string;
    };
} | {
    Burn: {
        token_id: string;
        burner: string;
    };
} | {
    Use: {
        token_id: string;
        user: string;
        action_type: string;
    };
};
/** Mise à jour de configuration */
type ConfigUpdate = {
    SetFeeRate: {
        bps: number;
    };
} | {
    SetPlatformFee: {
        bps: number;
    };
} | {
    SetNodeFee: {
        bps: number;
    };
} | {
    SetMinPow: {
        bits: number;
    };
} | {
    SetMintEnabled: {
        enabled: boolean;
    };
};
/** Bloc du DAG */
interface Block {
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
interface WireBlock {
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
/** Informations sur le supply */
interface SupplyInfo {
    /** Total en circulation */
    circulating: string;
    /** Nombre d'UTXOs */
    utxo_count: number;
}
/** Réponse de soumission de bloc */
interface SubmitResponse {
    /** Statut: "inserted" ou "already_exists" */
    status: "inserted" | "already_exists";
    /** ID du bloc */
    block_id: string;
}
/** Balance d'une adresse */
interface BalanceInfo {
    /** Adresse */
    address: string;
    /** Balance totale */
    balance: string;
    /** Liste des UTXOs */
    utxos: Utxo[];
}
/** Configuration du client PMS */
interface PmsClientConfig {
    /** URL du nœud (ex: "https://node.pms.network") */
    nodeUrl: string;
    /** ID du réseau (défaut: "pms-mainnet") */
    networkId?: string;
    /** Version du protocole (défaut: 1) */
    protocolVersion?: number;
    /** Timeout en ms (défaut: 30000) */
    timeout?: number;
}
/** Configuration par défaut */
declare const DEFAULT_CONFIG: Required<Omit<PmsClientConfig, "nodeUrl">>;

/**
 * PmsClient - Client HTTP pour interagir avec un nœud PMS.
 *
 * @example
 * ```typescript
 * const client = new PmsClient({ nodeUrl: "https://node.pms.network" });
 *
 * // Lecture
 * const tips = await client.getTips();
 * const balance = await client.getBalance("04abc...");
 *
 * // Écriture
 * const result = await client.send({
 *   to: "04def...",
 *   amount: "10.0",
 *   wallet: myWallet,
 * });
 * ```
 */

/**
 * Client pour interagir avec l'API REST d'un nœud PMS.
 */
declare class PmsClient {
    private readonly config;
    /**
     * Crée un nouveau client PMS.
     * @param config - Configuration du client
     */
    constructor(config: PmsClientConfig);
    /**
     * Récupère les tips actuels du DAG.
     */
    getTips(): Promise<string[]>;
    /**
     * Récupère un bloc par son ID.
     */
    getBlock(blockId: string): Promise<Block>;
    /**
     * Récupère les informations de supply.
     */
    getSupply(): Promise<SupplyInfo>;
    /**
     * Récupère les UTXOs d'une adresse.
     */
    getUtxos(address: string): Promise<Utxo[]>;
    /**
     * Récupère la balance d'une adresse.
     */
    getBalance(address: string): Promise<string>;
    /**
     * Récupère la balance complète avec les UTXOs.
     */
    getBalanceInfo(address: string): Promise<BalanceInfo>;
    /**
     * Soumet un bloc au réseau.
     */
    submitBlock(wireBlock: WireBlock): Promise<SubmitResponse>;
    /**
     * Envoie des tokens à une adresse.
     * Construit automatiquement la transaction, la signe et la soumet.
     */
    send(params: {
        to: string;
        amount: string;
        wallet: PmsWallet;
        memo?: string;
    }): Promise<SubmitResponse>;
    private fetch;
}

/**
 * Convertit des bytes en hex.
 */
declare function toHex(bytes: Uint8Array): string;
/**
 * Convertit un hex en bytes.
 */
declare function fromHex(hex: string): Uint8Array;
/**
 * Génère un ID de bloc à partir du contenu.
 * Format: SHA256(parents + payload + nonce)
 */
declare function computeBlockId(parents: string[], payloadJson: string | undefined, nonce: number): string;
/**
 * Vérifie si un ID a le nombre requis de leading zero bits (PoW).
 */
declare function checkPowBits(blockId: string, requiredBits: number): boolean;
/**
 * Parse un montant décimal en satoshis (8 décimales).
 */
declare function parseAmount(amount: string): bigint;
/**
 * Formate des satoshis en montant décimal.
 */
declare function formatAmount(sats: bigint): string;

export { type BalanceInfo, type Block, type ConfigUpdate, DEFAULT_CONFIG, type EncryptedPayload, type MilestonePayload, type MintPayload, type NftAction, type OutputRef, type PayloadEnvelope, type PlainPayload, PmsClient, type PmsClientConfig, PmsWallet, type SubmitResponse, type SupplyInfo, type TxOutput, type TxUtxo, type Utxo, type WireBlock, checkPowBits, computeBlockId, formatAmount, fromHex, isValidMnemonic, parseAmount, toHex };
