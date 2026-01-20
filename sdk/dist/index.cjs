"use strict";
var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};
var __copyProps = (to, from, except, desc) => {
  if (from && typeof from === "object" || typeof from === "function") {
    for (let key of __getOwnPropNames(from))
      if (!__hasOwnProp.call(to, key) && key !== except)
        __defProp(to, key, { get: () => from[key], enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable });
  }
  return to;
};
var __toCommonJS = (mod) => __copyProps(__defProp({}, "__esModule", { value: true }), mod);

// src/index.ts
var index_exports = {};
__export(index_exports, {
  DEFAULT_CONFIG: () => DEFAULT_CONFIG,
  PmsClient: () => PmsClient,
  PmsWallet: () => PmsWallet,
  checkPowBits: () => checkPowBits,
  computeBlockId: () => computeBlockId,
  decryptPayload: () => decryptPayload,
  deriveX25519PublicKey: () => deriveX25519PublicKey,
  encryptPayload: () => encryptPayload,
  formatAmount: () => formatAmount,
  formatCubeAttributesMessage: () => formatCubeAttributesMessage,
  fromHex: () => fromHex,
  generateX25519Keypair: () => generateX25519Keypair,
  isValidMnemonic: () => isValidMnemonic,
  parseAmount: () => parseAmount,
  signCubeAttributes: () => signCubeAttributes,
  toHex: () => toHex
});
module.exports = __toCommonJS(index_exports);

// src/wallet.ts
var import_secp256k1 = require("@noble/curves/secp256k1");
var import_ed25519 = require("@noble/curves/ed25519");
var import_sha22 = require("@noble/hashes/sha2");
var import_hkdf = require("@noble/hashes/hkdf");
var import_bip39 = require("@scure/bip39");
var import_english = require("@scure/bip39/wordlists/english");

// src/utils.ts
var import_sha2 = require("@noble/hashes/sha2");
var import_utils = require("@noble/hashes/utils");
function sha256Hash(data) {
  return (0, import_sha2.sha256)(data);
}
function toHex(bytes) {
  return (0, import_utils.bytesToHex)(bytes);
}
function fromHex(hex) {
  return (0, import_utils.hexToBytes)(hex);
}
function encodeUtf8(str) {
  return new TextEncoder().encode(str);
}
function computeBlockId(parents, payloadJson, nonce) {
  const parentsStr = parents.sort().join(",");
  const payloadStr = payloadJson ?? "";
  const content = `${parentsStr}|${payloadStr}|${nonce}`;
  const hash = sha256Hash(encodeUtf8(content));
  return toHex(hash);
}
function checkPowBits(blockId, requiredBits) {
  if (requiredBits === 0) return true;
  const bytes = fromHex(blockId);
  let zeroBits = 0;
  for (const byte of bytes) {
    if (byte === 0) {
      zeroBits += 8;
    } else {
      let mask = 128;
      while (mask > 0 && (byte & mask) === 0) {
        zeroBits++;
        mask >>= 1;
      }
      break;
    }
  }
  return zeroBits >= requiredBits;
}
function parseAmount(amount) {
  const [whole, frac = ""] = amount.split(".");
  const fracPadded = frac.padEnd(8, "0").slice(0, 8);
  return BigInt(whole) * 100000000n + BigInt(fracPadded);
}
function formatAmount(sats) {
  const whole = sats / 100000000n;
  const frac = sats % 100000000n;
  const fracStr = frac.toString().padStart(8, "0");
  return `${whole}.${fracStr}`;
}

// src/wallet.ts
var PmsWallet = class _PmsWallet {
  /** Clé privée secp256k1 (32 bytes) - pour signatures */
  _privateKey;
  /** Clé publique secp256k1 non compressée (65 bytes: 04 + x + y) */
  _publicKey;
  /** Clé privée X25519 (32 bytes) - pour chiffrement */
  _x25519PrivateKey;
  /** Clé publique X25519 (32 bytes) */
  _x25519PublicKey;
  /** Phrase mnémonique (24 mots) si générée/importée */
  _mnemonic;
  /**
   * Constructeur privé - utiliser les méthodes statiques.
   */
  constructor(privateKey, mnemonic) {
    this._privateKey = privateKey;
    this._publicKey = import_secp256k1.secp256k1.getPublicKey(privateKey, false);
    this._x25519PrivateKey = (0, import_hkdf.hkdf)(
      import_sha22.sha256,
      privateKey,
      new TextEncoder().encode("pms-x25519"),
      // salt
      new TextEncoder().encode("encryption"),
      // info
      32
    );
    this._x25519PublicKey = import_ed25519.x25519.getPublicKey(this._x25519PrivateKey);
    this._mnemonic = mnemonic;
  }
  // ═══════════════════════════════════════════════════════════════════════
  // Méthodes statiques de création
  // ═══════════════════════════════════════════════════════════════════════
  /**
   * Génère un nouveau wallet avec une phrase de 24 mots.
   */
  static generate() {
    const mnemonic = (0, import_bip39.generateMnemonic)(import_english.wordlist, 256);
    return _PmsWallet.fromMnemonic(mnemonic);
  }
  /**
   * Crée un wallet à partir d'une phrase mnémonique (12, 15, 18, 21 ou 24 mots).
   * @throws Error si la phrase est invalide
   */
  static fromMnemonic(mnemonic) {
    const normalized = mnemonic.trim().toLowerCase();
    if (!(0, import_bip39.validateMnemonic)(normalized, import_english.wordlist)) {
      throw new Error("Invalid mnemonic phrase");
    }
    const seed = (0, import_bip39.mnemonicToSeedSync)(normalized);
    const privateKey = seed.slice(0, 32);
    return new _PmsWallet(privateKey, normalized);
  }
  /**
   * Crée un wallet à partir d'une clé privée hexadécimale.
   */
  static fromPrivateKey(privateKeyHex) {
    const privateKey = fromHex(privateKeyHex);
    if (privateKey.length !== 32) {
      throw new Error("Private key must be 32 bytes");
    }
    return new _PmsWallet(privateKey);
  }
  /**
   * Crée un wallet à partir d'une seed (32 bytes).
   */
  static fromSeed(seed) {
    if (seed.length !== 32) {
      throw new Error("Seed must be 32 bytes");
    }
    return new _PmsWallet(seed);
  }
  // ═══════════════════════════════════════════════════════════════════════
  // Propriétés publiques
  // ═══════════════════════════════════════════════════════════════════════
  /**
   * Adresse du wallet (clé publique hex).
   * Format: "04" + 64 bytes hex = 130 caractères
   */
  get address() {
    return toHex(this._publicKey);
  }
  /**
   * Clé publique en bytes.
   */
  get publicKey() {
    return this._publicKey;
  }
  /**
   * Clé publique en hex.
   */
  get publicKeyHex() {
    return toHex(this._publicKey);
  }
  /**
   * Phrase mnémonique (si disponible).
   * @returns undefined si le wallet a été créé depuis une clé privée
   */
  get mnemonic() {
    return this._mnemonic;
  }
  // ═══════════════════════════════════════════════════════════════════════
  // Clés X25519 (pour chiffrement)
  // ═══════════════════════════════════════════════════════════════════════
  /**
   * Clé publique X25519 en hex (pour chiffrement).
   * Utiliser cette clé comme destinataire pour encryptPayload().
   */
  get x25519PublicKeyHex() {
    return toHex(this._x25519PublicKey);
  }
  /**
   * Clé privée X25519 en hex (pour déchiffrement).
   * ⚠️ Ne pas exposer cette clé publiquement !
   */
  get x25519PrivateKeyHex() {
    return toHex(this._x25519PrivateKey);
  }
  // ═══════════════════════════════════════════════════════════════════════
  // Méthodes de signature
  // ═══════════════════════════════════════════════════════════════════════
  /**
   * Signe un message avec la clé privée.
   * @param message - Message à signer (sera hashé avec SHA256)
   * @returns Signature DER encodée en hex
   */
  sign(message) {
    const hash = (0, import_sha22.sha256)(message);
    const sig = import_secp256k1.secp256k1.sign(hash, this._privateKey);
    return sig.toDERHex();
  }
  /**
   * Signe un message déjà hashé.
   * @param hash - Hash 32 bytes du message
   * @returns Signature DER encodée en hex
   */
  signHash(hash) {
    if (hash.length !== 32) {
      throw new Error("Hash must be 32 bytes");
    }
    const sig = import_secp256k1.secp256k1.sign(hash, this._privateKey);
    return sig.toDERHex();
  }
  /**
   * Exporte la clé privée en hex.
   * ⚠️ À utiliser avec précaution !
   */
  exportPrivateKey() {
    return toHex(this._privateKey);
  }
  // ═══════════════════════════════════════════════════════════════════════
  // Méthodes statiques de vérification
  // ═══════════════════════════════════════════════════════════════════════
  /**
   * Vérifie une signature.
   * @param message - Message original
   * @param signature - Signature DER hex
   * @param publicKeyHex - Clé publique hex du signataire
   */
  static verify(message, signature, publicKeyHex) {
    try {
      const hash = (0, import_sha22.sha256)(message);
      const pubKey = fromHex(publicKeyHex);
      const sig = import_secp256k1.secp256k1.Signature.fromDER(signature);
      return import_secp256k1.secp256k1.verify(sig.toCompactRawBytes(), hash, pubKey);
    } catch {
      return false;
    }
  }
};
function isValidMnemonic(mnemonic) {
  return (0, import_bip39.validateMnemonic)(mnemonic.trim().toLowerCase(), import_english.wordlist);
}

// src/types.ts
var DEFAULT_CONFIG = {
  networkId: "pms-mainnet",
  protocolVersion: 1,
  timeout: 3e4,
  seedNodes: [],
  enableRacing: true
};

// src/client.ts
var PmsClient = class {
  config;
  knownNodes = /* @__PURE__ */ new Set();
  lastNodeRefresh = 0;
  NODE_REFRESH_INTERVAL = 6e4;
  // 1 min
  /**
   * Crée un nouveau client PMS.
   * @param config - Configuration du client
   * @param config.nodeUrl - URL du nœud principal
   * @param config.enableRacing - Activer le racing pattern
   * @param config.seedNodes - Liste des nœuds de seed
   */
  constructor(config) {
    this.config = {
      ...DEFAULT_CONFIG,
      seedNodes: config.seedNodes ?? [],
      enableRacing: config.enableRacing ?? true,
      ...config
    };
    this.addKnownNode(this.config.nodeUrl);
    this.config.seedNodes.forEach((url) => this.addKnownNode(url));
  }
  addKnownNode(url) {
    const normalized = url.replace(/\/$/, "");
    if (normalized.startsWith("http")) {
      this.knownNodes.add(normalized);
    }
  }
  // ═══════════════════════════════════════════════════════════════════════
  // Méthodes de lecture (GET)
  // ═══════════════════════════════════════════════════════════════════════
  /**
   * Récupère les tips actuels du DAG.
   */
  async getTips() {
    const res = await this.fetch("/v1/dag/tips", {
      method: "POST",
      body: JSON.stringify({ limit: 10 })
    });
    return res;
  }
  /**
   * Récupère les informations publiques du coordinateur (clés).
   */
  async getCoordinatorInfo() {
    return this.fetch("/v1/coordinator/info");
  }
  /**
   * Récupère un bloc par son ID.
   */
  async getBlock(blockId) {
    return this.fetch(`/v1/blocks/${blockId}`);
  }
  /**
   * Récupère les informations de supply.
   */
  async getSupply() {
    return this.fetch("/v1/supply");
  }
  /**
   * Récupère les UTXOs d'une adresse.
   */
  async getUtxos(address) {
    const res = await this.fetch(`/v1/wallet/${address}/utxos`);
    return res.utxos ?? [];
  }
  /**
   * Récupère la balance d'une adresse.
   */
  /**
   * Récupère la balance d'une adresse.
   */
  async getBalance(address) {
    const res = await this.fetch("/v1/balance", {
      method: "POST",
      body: JSON.stringify({ address })
    });
    return res.balance;
  }
  /**
   * Récupère la balance complète avec les UTXOs.
   */
  async getBalanceInfo(address) {
    const utxos = await this.getUtxos(address);
    let total = 0n;
    for (const utxo of utxos) {
      total += parseAmount(utxo.amount);
    }
    return {
      address,
      balance: formatAmount(total),
      utxos
    };
  }
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
  async getNfts(address) {
    const res = await this.fetch(`/v1/wallet/${address}/nfts`);
    return res.token_ids ?? [];
  }
  // ═══════════════════════════════════════════════════════════════════════
  // Méthodes d'écriture (POST)
  // ═══════════════════════════════════════════════════════════════════════
  /**
   * Soumet un bloc au réseau.
   * Utilise le racing pattern si activé pour envoyer à plusieurs noeuds.
   */
  async submitBlock(wireBlock) {
    if (this.config.enableRacing) {
      return this.submitBlockRacing(wireBlock);
    }
    return this.fetch("/submit/block", {
      method: "POST",
      body: JSON.stringify(wireBlock)
    });
  }
  /**
   * Discovery & Racing Pattern:
   * 1. Refresh node list if stale
   * 2. Send to all known nodes in parallel
   * 3. Return first success
   */
  async submitBlockRacing(wireBlock) {
    this.refreshNodeList().catch((err) => console.debug("Node refresh failed:", err));
    const targets = Array.from(this.knownNodes);
    if (targets.length === 0) {
      targets.push(this.config.nodeUrl.replace(/\/$/, ""));
    }
    const body = JSON.stringify(wireBlock);
    const controller = new AbortController();
    const promises = targets.map(async (baseUrl) => {
      try {
        const res = await this.fetchUrl(baseUrl, "/submit/block", {
          method: "POST",
          body,
          signal: controller.signal
        });
        return res;
      } catch (err) {
        throw err;
      }
    });
    try {
      const result = await Promise.any(promises);
      controller.abort();
      return result;
    } catch (error) {
      const aggregateError = error;
      const errors = aggregateError.errors || [];
      const errorMessages = errors.map((e) => e.message || String(e)).join("; ");
      throw new Error(`Submit failed on all ${targets.length} nodes. Errors: ${errorMessages}`);
    }
  }
  /**
   * Rafraîchit la liste des noeuds connus depuis le registre
   */
  async refreshNodeList() {
    if (Date.now() - this.lastNodeRefresh < this.NODE_REFRESH_INTERVAL) {
      return;
    }
    try {
      const res = await this.fetch("/v1/nodes");
      res.nodes.forEach((node) => {
        if (node.api_url) this.addKnownNode(node.api_url);
      });
      this.lastNodeRefresh = Date.now();
    } catch (e) {
    }
  }
  /**
   * Envoie des tokens à une adresse.
   * Construit automatiquement la transaction, la signe et la soumet.
   */
  async send(params) {
    const { to, amount, wallet, memo } = params;
    const utxos = await this.getUtxos(wallet.address);
    if (utxos.length === 0) {
      throw new Error("No UTXOs available");
    }
    const amountSats = parseAmount(amount);
    const feeRate = 100n;
    const fee = amountSats * feeRate / 10000n;
    const totalNeeded = amountSats + fee;
    let selectedSats = 0n;
    const inputs = [];
    for (const utxo of utxos) {
      inputs.push(utxo.outpoint);
      selectedSats += parseAmount(utxo.amount);
      if (selectedSats >= totalNeeded) break;
    }
    if (selectedSats < totalNeeded) {
      throw new Error(
        `Insufficient balance: have ${formatAmount(selectedSats)}, need ${formatAmount(totalNeeded)}`
      );
    }
    const outputs = [
      { address: to, amount: formatAmount(amountSats) }
    ];
    const change = selectedSats - amountSats - fee;
    if (change > 0n) {
      outputs.push({ address: wallet.address, amount: formatAmount(change) });
    }
    const tx = {
      inputs,
      outputs,
      fee: formatAmount(fee),
      data: memo
    };
    const tips = await this.getTips();
    const parents = tips.slice(0, 2);
    const payload = { Plain: { TxUtxo: tx } };
    const payloadJson = JSON.stringify(payload);
    let nonce = 0;
    let blockId = computeBlockId(parents, payloadJson, nonce);
    const messageToSign = encodeUtf8(blockId);
    const signature = await wallet.sign(messageToSign);
    const wireBlock = {
      id: blockId,
      parents,
      payload_json: payloadJson,
      nonce,
      network_id: this.config.networkId,
      protocol_version: this.config.protocolVersion,
      signer_pk_hex: wallet.publicKeyHex,
      signature_hex: signature
    };
    return this.submitBlock(wireBlock);
  }
  // ═══════════════════════════════════════════════════════════════════════
  // Méthodes NFT Cube (Burn et Mint spécialisé)
  // ═══════════════════════════════════════════════════════════════════════
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
  async burnNft(params) {
    const { tokenId, wallet } = params;
    const tips = await this.getTips();
    const parents = tips.slice(0, 2);
    const payload = {
      Plain: {
        Nft: {
          Burn: {
            token_id: tokenId,
            burner: wallet.address
          }
        }
      }
    };
    const payloadJson = JSON.stringify(payload);
    const nonce = 0;
    const blockId = computeBlockId(parents, payloadJson, nonce);
    const canonicalView = {
      id: blockId,
      parents,
      payload_json: payloadJson,
      nonce,
      network_id: this.config.networkId,
      protocol_version: this.config.protocolVersion,
      signer_pk_hex: wallet.publicKeyHex
    };
    const messageToSign = JSON.stringify(canonicalView);
    const signatureHex = wallet.sign(encodeUtf8(messageToSign));
    const signatureBytes = fromHex(signatureHex);
    const signatureB64 = btoa(String.fromCharCode(...signatureBytes));
    const burnRequest = {
      id: blockId,
      parents,
      payload_json: payloadJson,
      nonce,
      network_id: this.config.networkId,
      protocol_version: this.config.protocolVersion,
      signer_pk_hex: wallet.publicKeyHex,
      signature_hex: signatureB64
      // base64 malgré le nom "hex"
    };
    return this.fetch("/v1/nft/burn", {
      method: "POST",
      body: JSON.stringify(burnRequest)
    });
  }
  /**
   * Mint un NFT via le Coordinateur (Server-Side Signing).
   * Le client génère l'ID et les métadonnées, mais c'est le serveur qui signe et chiffre.
   */
  async mintNft(params) {
    const { wallet, metadata } = params;
    const tokenId = params.tokenId || this.generateRandomHex(32);
    const response = await this.fetch("/v1/nft/mint", {
      method: "POST",
      body: JSON.stringify({
        token_id: tokenId,
        owner_address: wallet.address,
        owner_x25519_pubkey: wallet.x25519PublicKeyHex,
        metadata
      })
    });
    return { ...response, token_id: tokenId };
  }
  /**
   * Mint un Cube avec des attributs générés et signés par le backend Authority.
   * @param params.wallet - Wallet PMS du propriétaire
   * @param params.generatorUrl - URL du backend générateur de cubes (ex: "http://localhost:3000")
   */
  async mintCube(params) {
    const { wallet, generatorUrl } = params;
    const cubeResponse = await fetch(`${generatorUrl}/api/cube/generate`, {
      method: "POST"
    });
    if (!cubeResponse.ok) {
      const text = await cubeResponse.text();
      throw new Error(`Cube generation failed: ${cubeResponse.status} - ${text}`);
    }
    const cubeData = await cubeResponse.json();
    const tokenId = this.generateRandomHex(32);
    const metadata = {
      name: `Cube ${cubeData.rarity}`,
      description: `A ${cubeData.rarity} cube with unique properties.`,
      nft_type: "cube",
      extra: JSON.stringify({
        rarity: cubeData.rarity,
        attributes: cubeData.attributes,
        roll: cubeData.roll,
        signature: cubeData.signature
        // Authority signature
      })
    };
    const submitResult = await this.mintNft({
      wallet,
      metadata,
      tokenId
    });
    return {
      ...submitResult,
      rarity: cubeData.rarity,
      roll: cubeData.roll,
      attributes: cubeData.attributes
    };
  }
  /**
   * Génère une chaîne hexadécimale aléatoire de la longueur spécifiée (en bytes).
   * Utilise crypto.getRandomValues pour la sécurité cryptographique.
   */
  generateRandomHex(bytes) {
    const array = new Uint8Array(bytes);
    crypto.getRandomValues(array);
    return Array.from(array).map((b) => b.toString(16).padStart(2, "0")).join("");
  }
  // ═══════════════════════════════════════════════════════════════════════
  // Helper HTTP
  // ═══════════════════════════════════════════════════════════════════════
  async fetch(path, init) {
    return this.fetchUrl(this.config.nodeUrl, path, init);
  }
  async fetchUrl(baseUrl, path, init) {
    const url = `${baseUrl.replace(/\/$/, "")}/${path.replace(/^\//, "")}`;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), this.config.timeout);
    const signal = init?.signal || controller.signal;
    try {
      const res = await fetch(url, {
        ...init,
        headers: {
          "Content-Type": "application/json",
          ...init?.headers
        },
        signal
      });
      if (!res.ok) {
        const text = await res.text();
        throw new Error(`HTTP ${res.status} (${url}): ${text}`);
      }
      return res.json();
    } finally {
      clearTimeout(timeout);
    }
  }
};

// src/crypto.ts
var import_ed255192 = require("@noble/curves/ed25519");
var import_aes = require("@noble/ciphers/aes.js");
var import_hkdf2 = require("@noble/hashes/hkdf");
var import_sha23 = require("@noble/hashes/sha2");
var import_utils4 = require("@noble/hashes/utils");
var import_base = require("@scure/base");
var SCHEME = "x25519+aes256gcm";
var KEY_VERSION = 1;
var HKDF_SALT = new TextEncoder().encode("pms-dek-wrap");
var HKDF_INFO_KEK = new TextEncoder().encode("kek-v1");
var HKDF_INFO_KID = new TextEncoder().encode("kid-v1");
function toBase64(data) {
  return import_base.base64.encode(data);
}
function fromBase64(str) {
  return import_base.base64.decode(str);
}
function sha256Hex(data) {
  return (0, import_utils4.bytesToHex)((0, import_sha23.sha256)(data));
}
function encryptPayload(plaintext, recipientPublicKeysHex) {
  const plaintextBytes = typeof plaintext === "string" ? new TextEncoder().encode(plaintext) : plaintext;
  const dek = (0, import_utils4.randomBytes)(32);
  const nonce = (0, import_utils4.randomBytes)(12);
  const aad = { len_hint: plaintextBytes.length };
  const aadBytes = new TextEncoder().encode(JSON.stringify(aad));
  const cipher = (0, import_aes.gcm)(dek, nonce, aadBytes);
  const ciphertext = cipher.encrypt(plaintextBytes);
  const commitment = sha256Hex(plaintextBytes);
  const ephemeralPrivateKey = (0, import_utils4.randomBytes)(32);
  const ephemeralPublicKey = import_ed255192.x25519.getPublicKey(ephemeralPrivateKey);
  const ephemeralPublicKeyHex = (0, import_utils4.bytesToHex)(ephemeralPublicKey);
  const recipients = [];
  for (const recipientPkHex of recipientPublicKeysHex) {
    const recipientPk = (0, import_utils4.hexToBytes)(recipientPkHex);
    const sharedSecret = import_ed255192.x25519.getSharedSecret(ephemeralPrivateKey, recipientPk);
    const kek = (0, import_hkdf2.hkdf)(import_sha23.sha256, sharedSecret, HKDF_SALT, HKDF_INFO_KEK, 32);
    const kidBytes = (0, import_hkdf2.hkdf)(import_sha23.sha256, sharedSecret, HKDF_SALT, HKDF_INFO_KID, 16);
    const kid = (0, import_utils4.bytesToHex)(kidBytes);
    const kwNonce = (0, import_utils4.randomBytes)(12);
    const kwCipher = (0, import_aes.gcm)(kek, kwNonce, new TextEncoder().encode(kid));
    const wrappedKey = kwCipher.encrypt(dek);
    recipients.push({
      kid,
      ephem_pub: ephemeralPublicKeyHex,
      wrapped_key_b64: toBase64(wrappedKey),
      kw_nonce_b64: toBase64(kwNonce)
    });
  }
  return {
    scheme: SCHEME,
    key_version: KEY_VERSION,
    aad,
    commitment,
    ciphertext_b64: toBase64(ciphertext),
    recipients,
    nonce_b64: toBase64(nonce)
  };
}
function decryptPayload(encrypted, recipientPrivateKeyHex) {
  if (encrypted.scheme !== SCHEME) {
    throw new Error(`Sch\xE9ma non support\xE9: ${encrypted.scheme}`);
  }
  const recipientSk = (0, import_utils4.hexToBytes)(recipientPrivateKeyHex);
  let dek = null;
  for (const wrap of encrypted.recipients) {
    const ephemeralPk = (0, import_utils4.hexToBytes)(wrap.ephem_pub);
    const sharedSecret = import_ed255192.x25519.getSharedSecret(recipientSk, ephemeralPk);
    const kek = (0, import_hkdf2.hkdf)(import_sha23.sha256, sharedSecret, HKDF_SALT, HKDF_INFO_KEK, 32);
    const kidBytes = (0, import_hkdf2.hkdf)(import_sha23.sha256, sharedSecret, HKDF_SALT, HKDF_INFO_KID, 16);
    const expectedKid = (0, import_utils4.bytesToHex)(kidBytes);
    if (expectedKid !== wrap.kid) {
      continue;
    }
    try {
      const kwNonce = fromBase64(wrap.kw_nonce_b64);
      const wrappedKey = fromBase64(wrap.wrapped_key_b64);
      const kwCipher = (0, import_aes.gcm)(kek, kwNonce, new TextEncoder().encode(wrap.kid));
      dek = kwCipher.decrypt(wrappedKey);
      break;
    } catch {
      continue;
    }
  }
  if (!dek) {
    throw new Error("Aucun destinataire correspondant trouv\xE9 ou d\xE9ballage \xE9chou\xE9");
  }
  const nonce = fromBase64(encrypted.nonce_b64);
  const ciphertext = fromBase64(encrypted.ciphertext_b64);
  const aadBytes = new TextEncoder().encode(JSON.stringify(encrypted.aad));
  const cipher = (0, import_aes.gcm)(dek, nonce, aadBytes);
  const plaintext = cipher.decrypt(ciphertext);
  const gotCommitment = sha256Hex(plaintext);
  if (gotCommitment !== encrypted.commitment) {
    throw new Error("Commitment mismatch - donn\xE9es corrompues");
  }
  return new TextDecoder().decode(plaintext);
}
function generateX25519Keypair() {
  const privateKey = (0, import_utils4.randomBytes)(32);
  const publicKey = import_ed255192.x25519.getPublicKey(privateKey);
  return {
    privateKey: (0, import_utils4.bytesToHex)(privateKey),
    publicKey: (0, import_utils4.bytesToHex)(publicKey)
  };
}
function deriveX25519PublicKey(privateKeyHex) {
  const privateKey = (0, import_utils4.hexToBytes)(privateKeyHex);
  const publicKey = import_ed255192.x25519.getPublicKey(privateKey);
  return (0, import_utils4.bytesToHex)(publicKey);
}
function formatCubeAttributesMessage(weight, size, density) {
  return `weight:${weight},size:${size},density:${density}`;
}
function signCubeAttributes(weight, size, density, authorityWallet) {
  const message = formatCubeAttributesMessage(weight, size, density);
  const messageBytes = new TextEncoder().encode(message);
  const signatureHex = authorityWallet.sign(messageBytes);
  const signatureBytes = (0, import_utils4.hexToBytes)(signatureHex);
  return toBase64(signatureBytes);
}
// Annotate the CommonJS export names for ESM import in node:
0 && (module.exports = {
  DEFAULT_CONFIG,
  PmsClient,
  PmsWallet,
  checkPowBits,
  computeBlockId,
  decryptPayload,
  deriveX25519PublicKey,
  encryptPayload,
  formatAmount,
  formatCubeAttributesMessage,
  fromHex,
  generateX25519Keypair,
  isValidMnemonic,
  parseAmount,
  signCubeAttributes,
  toHex
});
