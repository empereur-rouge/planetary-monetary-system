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
    /** Clé privée secp256k1 (32 bytes) - pour signatures */
    private readonly _privateKey;
    /** Clé publique secp256k1 non compressée (65 bytes: 04 + x + y) */
    private readonly _publicKey;
    /** Clé privée X25519 (32 bytes) - pour chiffrement */
    private readonly _x25519PrivateKey;
    /** Clé publique X25519 (32 bytes) */
    private readonly _x25519PublicKey;
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
     * Clé publique X25519 en hex (pour chiffrement).
     * Utiliser cette clé comme destinataire pour encryptPayload().
     */
    get x25519PublicKeyHex(): string;
    /**
     * Clé privée X25519 en hex (pour déchiffrement).
     * ⚠️ Ne pas exposer cette clé publiquement !
     */
    get x25519PrivateKeyHex(): string;
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
/** Input de transaction (wrapper autour de OutputRef) */
interface TxInput {
    /** Référence à l'UTXO dépensé */
    out: OutputRef;
}
/** Unlock (signature pour un input) */
interface Unlock {
    /** Clé publique hex */
    pubkey_hex: string;
    /** Signature base64 */
    signature_b64: string;
}
/** Transaction UTXO */
interface TxUtxo {
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
} | {
    Reward: RewardPayload;
} | {
    EncryptedReward: EncryptedRewardPayload;
};
/** Payload de récompense (Mining/Fees) */
interface RewardPayload {
    fee_outputs: TaggedOutput[];
    reward_outputs: TxOutput[];
    burned: string;
    tx_block_id: string;
}
/** Output avec tag (pour fees) */
interface TaggedOutput extends TxOutput {
    tag: string;
}
/** Payload de récompense chiffrée */
interface EncryptedRewardPayload {
    encrypted_outputs: EncryptedRewardOutput[];
    burned: string;
    tx_block_id: string;
}
/** Output de récompensation chiffré individuellement */
interface EncryptedRewardOutput {
    encrypted: EncryptedPayload;
}
/** Payload chiffré - compatible avec pms-types-payload/encrypted_payload.rs */
interface EncryptedPayload {
    /** Schéma de chiffrement (toujours "x25519+aes256gcm") */
    scheme: string;
    /** Version de la clé (pour rotation future) */
    key_version: number;
    /** Métadonnée authentifiée (taille du payload) */
    aad: {
        len_hint: number;
    };
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
interface KeyWrap {
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
/**
 * Métadonnées pour un NFT.
 *
 * Ces champs sont tous optionnels pour offrir de la flexibilité,
 * mais au minimum `name` ou `uri` devraient être renseignés.
 */
interface NftMetadata {
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
type NftAction = {
    Mint: {
        token_id: string;
        owner: string;
        metadata: NftMetadata;
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
    BatchBurn: {
        token_ids: string[];
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
/** Attributs générés d'un Cube */
interface CubeAttributes {
    /** Poids (1-100, hautes valeurs = rares) */
    weight: number;
    /** Taille (1-100, hautes valeurs = rares) */
    size: number;
    /** Densité (1-100, hautes valeurs = rares) */
    density: number;
}
/** Réponse de mintCube - inclut les données du cube généré */
interface MintCubeResponse extends SubmitResponse {
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
interface RefundPreview {
    /** Montant du remboursement en PMS */
    amount: string;
    /** Adresse destinataire du remboursement */
    recipient: string;
}
/** Réponse de burnNft - inclut le refund preview si cube authentique */
interface BurnNftResponse {
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
/** Réponse de getNft - informations complètes d'un NFT */
interface NftResponse {
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
/** Réponse de l'endpoint d'information du coordinateur */
interface CoordinatorInfoResponse {
    /** Ce nœud est-il le Coordinateur ? */
    is_coordinator: boolean;
    /** Clé publique secp256k1 (hex) - pour vérifier les signatures */
    secp256k1_pubkey: string;
    /** Clé publique X25519 (hex) - pour le chiffrement */
    x25519_pubkey: string;
}
/** Élément d'historique de wallet */
interface HistoryItem {
    block_id: string;
    ts_ms: number;
    payload_type: string;
    payload: any;
}
/** Réponse de l'historique du wallet */
interface WalletHistoryResp {
    address: string;
    items: HistoryItem[];
    count: number;
}
interface RuntimeConfig {
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
interface NodePublicConfig extends RuntimeConfig {
    fee_recipient: string;
}

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
    private knownNodes;
    private lastNodeRefresh;
    private readonly NODE_REFRESH_INTERVAL;
    private configCache;
    private readonly CONFIG_TTL;
    /**
     * Crée un nouveau client PMS.
     * @param config - Configuration du client
     * @param config.nodeUrl - URL du nœud principal
     * @param config.enableRacing - Activer le racing pattern
     * @param config.seedNodes - Liste des nœuds de seed
     */
    constructor(config: PmsClientConfig);
    /**
     * @internal
     * Ajoute un nœud à la liste des nœuds connus.
     */
    private addKnownNode;
    /**
     * Récupère les tips actuels du DAG.
     */
    getTips(): Promise<string[]>;
    /**
     * Récupère les informations publiques du coordinateur (clés).
     */
    getCoordinatorInfo(): Promise<CoordinatorInfoResponse>;
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
    /**
     * Récupère la balance d'une adresse.
     */
    getBalance(address: string): Promise<string>;
    /**
     * Récupère la balance complète avec les UTXOs.
     */
    getBalanceInfo(address: string): Promise<BalanceInfo>;
    /**
     * Récupère la liste des NFTs appartenant à une adresse.
     *
     * @param address - Adresse publique (hex) du propriétaire
     * @returns Liste des token_ids possédés par cette adresse
     *
     * @example
     * ```typescript
     * const myNfts = await client.getNfts(myWallet.address);
     * console.log(`Vous possédez ${myNfts.length} NFT(s)`);
     * for (const tokenId of myNfts) {
     *     console.log(`- ${tokenId}`);
     * }
     * ```
     */
    getNfts(address: string): Promise<string[]>;
    /**
     * Récupère les informations complètes d'un NFT par son token_id.
     *
     * @param tokenId - Identifiant unique du NFT (64 caractères hex)
     * @returns NftResponse avec owner, exists et metadata
     *
     * @example
     * ```typescript
     * const nft = await client.getNft("abc123def456...");
     * if (nft.exists) {
     *     console.log(`Owner: ${nft.owner}`);
     *     console.log(`Name: ${nft.metadata?.name}`);
     * }
     * ```
     */
    getNft(tokenId: string): Promise<NftResponse>;
    /**
     * Récupère l'historique des transactions d'un wallet.
     * Supporte le déchiffrement des récompenses (EncryptedReward) côté client.
     *
     * @param address - Adresse du wallet (bech32)
     * @param options - Options (limit, decryptionWallet)
     * @returns Historique complet
     */
    getHistory(address: string, options?: {
        limit?: number;
        decryptionWallet?: PmsWallet;
    }): Promise<WalletHistoryResp>;
    /**
     * Récupère la configuration publique du nœud (frais, PoW, recipient, etc.)
     */
    getNetworkConfig(): Promise<NodePublicConfig>;
    /**
     * Récupère la configuration runtime du nœud (frais, PoW, etc.)
     * @deprecated Use getNetworkConfig instead
     */
    getRuntimeConfig(): Promise<RuntimeConfig>;
    /**
     * @internal
     * Soumet un bloc au réseau.
     * Utilise le racing pattern si activé pour envoyer à plusieurs noeuds.
     *
     * ⚠️ API interne - préférez utiliser `send()`, `mintNft()`, ou `burnNft()`.
     */
    submitBlock(wireBlock: WireBlock): Promise<SubmitResponse>;
    /**
     * Discovery & Racing Pattern:
     * 1. Refresh node list if stale
     * 2. Send to all known nodes in parallel
     * 3. Return first success
     */
    private submitBlockRacing;
    /**
     * Rafraîchit la liste des noeuds connus depuis le registre
     */
    private refreshNodeList;
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
    /**
     * Transférer un NFT à un autre propriétaire.
     * Prend en charge le re-chiffrement des métadonnées via le coordinateur.
     *
     * @param params - Paramètres du transfert
     * @param params.tokenId - Identifiant du NFT
     * @param params.to - Adresse du nouveau propriétaire
     * @param params.wallet - Wallet du propriétaire actuel (signataire)
     * @param params.newOwnerX25519 - Clé publique X25519 du nouveau owner (pour re-encryption)
     */
    transferNft(params: {
        tokenId: string;
        to: string;
        wallet: PmsWallet;
        newOwnerX25519?: string;
    }): Promise<SubmitResponse>;
    /**
     * Brûle (détruit) un NFT existant.
     *
     * Seul le propriétaire du NFT peut le brûler.
     * Une fois brûlé, le NFT est supprimé définitivement.
     *
     * Pour les Cubes authentiques (avec signature Authority valide),
     * un remboursement est calculé selon la formule:
     * `(weight * size * density) / 10000` PMS
     *
     * @param params - Paramètres du burn
     * @param params.tokenId - Identifiant du NFT à brûler
     * @param params.wallet - Wallet PMS du propriétaire (doit être l'owner actuel)
     * @returns BurnNftResponse avec refund preview si cube authentique
     *
     * @example
     * ```typescript
     * const result = await client.burnNft({
     *     tokenId: "abc123def456...",
     *     wallet: myWallet,
     * });
     *
     * if (result.refund) {
     *     console.log(`Remboursement: ${result.refund.amount} PMS`);
     * }
     * ```
     */
    burnNft(params: {
        tokenId: string;
        wallet: PmsWallet;
    }): Promise<BurnNftResponse>;
    /**
     * Brûle (détruit) plusieurs NFTs en une seule transaction.
     *
     * @param params - Paramètres du batch burn
     * @param params.tokenIds - Liste des Identifiants des NFTs à brûler
     * @param params.wallet - Wallet PMS du propriétaire
     * @returns BurnNftResponse avec refund preview cumulé si applicable
     */
    burnNfts(params: {
        tokenIds: string[];
        wallet: PmsWallet;
    }): Promise<BurnNftResponse>;
    /**
     * Mint un NFT via le Coordinateur (Server-Side Signing).
     * Le client génère l'ID et les métadonnées, mais c'est le serveur qui signe et chiffre.
     */
    mintNft(params: {
        wallet: PmsWallet;
        metadata: NftMetadata;
        tokenId?: string;
    }): Promise<SubmitResponse & {
        token_id: string;
    }>;
    /**
     * Mint un Cube avec des attributs générés et signés par le backend Authority.
     * @param params.wallet - Wallet PMS du propriétaire
     * @param params.generatorUrl - URL du backend générateur de cubes (ex: "http://localhost:3000")
     */
    mintCube(params: {
        wallet: PmsWallet;
        generatorUrl: string;
    }): Promise<MintCubeResponse>;
    /**
     * Génère une chaîne hexadécimale aléatoire de la longueur spécifiée (en bytes).
     * Utilise crypto.getRandomValues pour la sécurité cryptographique.
     */
    private generateRandomHex;
    private fetch;
    private fetchUrl;
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
 * Parse un montant décimal en satoshis (8 décimales).
 */
declare function parseAmount(amount: string): bigint;
/**
 * Formate des satoshis en montant décimal.
 */
declare function formatAmount(sats: bigint): string;

/**
 * Fonctions de chiffrement pour le SDK PMS.
 *
 * Compatible avec le backend Rust (pms-types-payload/encrypted_payload.rs).
 * Schéma: X25519 + AES-256-GCM avec key wrapping multi-destinataires.
 *
 * CONCEPTS CLÉS (pour débutants Rust, applicable ici aussi):
 * - DEK (Data Encryption Key): Clé symétrique pour chiffrer les données
 * - KEK (Key Encryption Key): Clé pour "envelopper" la DEK
 * - ECDH: Échange de clé Diffie-Hellman sur courbe elliptique
 * - HKDF: Fonction de dérivation de clé à partir d'un secret partagé
 */

/**
 * Déchiffre un payload chiffré avec la clé privée X25519 du destinataire.
 *
 * @param encrypted - Payload chiffré (output de encryptPayload)
 * @param recipientPrivateKeyHex - Clé privée X25519 du destinataire (64 chars hex)
 * @returns Les données en clair (string)
 * @throws Error si le déchiffrement échoue
 *
 * @example
 * ```typescript
 * const plaintext = decryptPayload(encrypted, myX25519PrivateKeyHex);
 * const data = JSON.parse(plaintext);
 * ```
 */
declare function decryptPayload(encrypted: EncryptedPayload, recipientPrivateKeyHex: string): string;

export { type BalanceInfo, type Block, type BurnNftResponse, type CoordinatorInfoResponse, type CubeAttributes, type HistoryItem, type MintCubeResponse, type NftMetadata, type NftResponse, PmsClient, type PmsClientConfig, PmsWallet, type RuntimeConfig, type SubmitResponse, type SupplyInfo, type Utxo, type WalletHistoryResp, decryptPayload, formatAmount, fromHex, isValidMnemonic, parseAmount, toHex };
