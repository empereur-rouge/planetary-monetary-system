// src/wallet.ts
import { secp256k1 } from "@noble/curves/secp256k1";
import { sha256 as sha2562 } from "@noble/hashes/sha2";
import { generateMnemonic, mnemonicToSeedSync, validateMnemonic } from "@scure/bip39";
import { wordlist } from "@scure/bip39/wordlists/english";

// src/utils.ts
import { sha256 } from "@noble/hashes/sha2";
import { bytesToHex, hexToBytes } from "@noble/hashes/utils";
function sha256Hash(data) {
  return sha256(data);
}
function toHex(bytes) {
  return bytesToHex(bytes);
}
function fromHex(hex) {
  return hexToBytes(hex);
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
  /** Clé privée (32 bytes) */
  _privateKey;
  /** Clé publique non compressée (65 bytes: 04 + x + y) */
  _publicKey;
  /** Phrase mnémonique (24 mots) si générée/importée */
  _mnemonic;
  /**
   * Constructeur privé - utiliser les méthodes statiques.
   */
  constructor(privateKey, mnemonic) {
    this._privateKey = privateKey;
    this._publicKey = secp256k1.getPublicKey(privateKey, false);
    this._mnemonic = mnemonic;
  }
  // ═══════════════════════════════════════════════════════════════════════
  // Méthodes statiques de création
  // ═══════════════════════════════════════════════════════════════════════
  /**
   * Génère un nouveau wallet avec une phrase de 24 mots.
   */
  static generate() {
    const mnemonic = generateMnemonic(wordlist, 256);
    return _PmsWallet.fromMnemonic(mnemonic);
  }
  /**
   * Crée un wallet à partir d'une phrase mnémonique (12, 15, 18, 21 ou 24 mots).
   * @throws Error si la phrase est invalide
   */
  static fromMnemonic(mnemonic) {
    const normalized = mnemonic.trim().toLowerCase();
    if (!validateMnemonic(normalized, wordlist)) {
      throw new Error("Invalid mnemonic phrase");
    }
    const seed = mnemonicToSeedSync(normalized);
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
  // Méthodes de signature
  // ═══════════════════════════════════════════════════════════════════════
  /**
   * Signe un message avec la clé privée.
   * @param message - Message à signer (sera hashé avec SHA256)
   * @returns Signature DER encodée en hex
   */
  sign(message) {
    const hash = sha2562(message);
    const sig = secp256k1.sign(hash, this._privateKey);
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
    const sig = secp256k1.sign(hash, this._privateKey);
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
      const hash = sha2562(message);
      const pubKey = fromHex(publicKeyHex);
      const sig = secp256k1.Signature.fromDER(signature);
      return secp256k1.verify(sig.toCompactRawBytes(), hash, pubKey);
    } catch {
      return false;
    }
  }
};
function isValidMnemonic(mnemonic) {
  return validateMnemonic(mnemonic.trim().toLowerCase(), wordlist);
}

// src/types.ts
var DEFAULT_CONFIG = {
  networkId: "pms-mainnet",
  protocolVersion: 1,
  timeout: 3e4
};

// src/client.ts
var PmsClient = class {
  config;
  /**
   * Crée un nouveau client PMS.
   * @param config - Configuration du client
   */
  constructor(config) {
    this.config = {
      ...DEFAULT_CONFIG,
      ...config
    };
  }
  // ═══════════════════════════════════════════════════════════════════════
  // Méthodes de lecture (GET)
  // ═══════════════════════════════════════════════════════════════════════
  /**
   * Récupère les tips actuels du DAG.
   */
  async getTips() {
    const res = await this.fetch("/v1/tips");
    return res.tips;
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
  async getBalance(address) {
    const utxos = await this.getUtxos(address);
    let total = 0n;
    for (const utxo of utxos) {
      total += parseAmount(utxo.amount);
    }
    return formatAmount(total);
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
  // ═══════════════════════════════════════════════════════════════════════
  // Méthodes d'écriture (POST)
  // ═══════════════════════════════════════════════════════════════════════
  /**
   * Soumet un bloc au réseau.
   */
  async submitBlock(wireBlock) {
    return this.fetch("/v1/submit", {
      method: "POST",
      body: JSON.stringify(wireBlock)
    });
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
  // Helper HTTP
  // ═══════════════════════════════════════════════════════════════════════
  async fetch(path, init) {
    const url = `${this.config.nodeUrl}${path}`;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), this.config.timeout);
    try {
      const res = await fetch(url, {
        ...init,
        headers: {
          "Content-Type": "application/json",
          ...init?.headers
        },
        signal: controller.signal
      });
      if (!res.ok) {
        const text = await res.text();
        throw new Error(`HTTP ${res.status}: ${text}`);
      }
      return res.json();
    } finally {
      clearTimeout(timeout);
    }
  }
};
export {
  DEFAULT_CONFIG,
  PmsClient,
  PmsWallet,
  checkPowBits,
  computeBlockId,
  formatAmount,
  fromHex,
  isValidMnemonic,
  parseAmount,
  toHex
};
