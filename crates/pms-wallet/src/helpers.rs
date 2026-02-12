use crate::{Wallet, decode_address};
use anyhow::{Result, bail};
use futures::future::join_all;
use pms_storage::rocks_store::store::RocksStore;
use pms_token::{Amount, FeePolicy, PLANETARY_MONETARY_SYSTEM as PMS};
use pms_types_transaction::{OutputId, Transaction, TxInput, TxOutput};
use rand::distributions::WeightedIndex;
use rand::prelude::*;
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Erreurs possibles lors de la préparation d'une Transaction.
#[derive(Debug, Error)]
pub enum WalletTxError {
    #[error("montant invalide")]
    InvalidAmount,
    #[error("fee invalide")]
    FeeComputation,
    #[error("inputs insuffisants")]
    InsufficientInputs,
}

pub struct Payment {
    pub to: String,
    pub amount: String, // "10.00000000"
    /// Asset ID (None = PMS natif, Some("edenite") = token custom)
    pub asset_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SelectedInput {
    pub id: OutputId,
    pub amount: String,
} // montant de l’UTXO

pub fn build_utxo_tx_with_fee_checked(
    from_address: &str,
    inputs: Vec<SelectedInput>,
    payments: Vec<Payment>,
    fee_policy: &FeePolicy,
    fee_recipient: &str,
) -> Result<Transaction, WalletTxError> {
    let mut sum_out = Amount::parse("0", PMS.decimals).map_err(|_| WalletTxError::InvalidAmount)?;
    let mut outs = Vec::with_capacity(payments.len());
    for p in payments {
        let a = Amount::parse(&p.amount, PMS.decimals).map_err(|_| WalletTxError::InvalidAmount)?;
        sum_out.0 += a.0;
        outs.push(TxOutput {
            address: p.to,
            amount: a.to_string(),
            asset_id: p.asset_id,
        });
    }
    // compute_fee() retourne maintenant un Amount arrondi à 8 décimales
    let fee = fee_policy
        .compute_fee(&sum_out.to_string())
        .map_err(|_| WalletTxError::FeeComputation)?;

    if !fee.is_zero() {
        outs.push(TxOutput {
            address: fee_recipient.to_string(),
            amount: fee.to_string(),
            asset_id: None,
        });
    }

    let mut sum_in = Amount::parse("0", PMS.decimals).unwrap();
    let mut ins = Vec::with_capacity(inputs.len());
    for i in inputs {
        let ai =
            Amount::parse(&i.amount, PMS.decimals).map_err(|_| WalletTxError::InvalidAmount)?;
        sum_in.0 += ai.0;
        ins.push(TxInput { out: i.id });
    }

    // Vérif couverture
    if sum_in.0 < (sum_out.0 + fee.0) {
        return Err(WalletTxError::InsufficientInputs);
    }

    // Change si nécessaire
    let change = sum_in.0 - (sum_out.0 + fee.0);
    if !change.is_zero() {
        outs.push(TxOutput {
            address: from_address.to_string(),
            amount: Amount(change).to_string(),
            asset_id: None,
        });
    }

    Ok(Transaction {
        inputs: ins,
        outputs: outs,
        fee: fee.to_string(),
        unlocks: Vec::new(),
    })
}

/// Sélectionne une adresse admin avec proba ~ déficit vs moyenne, plancher epsilon.
/// `fetch_balance(addr)` doit retourner le solde courant de `addr` (Decimal).
pub async fn pick_admin_address_weighted<F, Fut>(
    admin_addrs: &[String],
    mut fetch_balance: F,
    epsilon: Decimal, // ex: Decimal::new(1, 3) == 0.001
) -> Result<String>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<Decimal>>,
{
    if admin_addrs.is_empty() {
        bail!("admin.wallet_addresses est vide");
    }

    // 1) Récupère tous les soldes en parallèle
    let balances_res = join_all(admin_addrs.iter().map(|a| fetch_balance(a))).await;

    let mut balances = Vec::with_capacity(admin_addrs.len());
    for (i, br) in balances_res.into_iter().enumerate() {
        let b = br.map_err(|e| anyhow::anyhow!("balance({}): {e}", admin_addrs[i]))?;
        balances.push(b.max(Decimal::ZERO));
    }

    // 2) Moyenne
    let sum: Decimal = balances.iter().copied().sum();
    let n = Decimal::from(admin_addrs.len() as u64);
    let avg = if n.is_zero() { Decimal::ZERO } else { sum / n };

    // 3) Poids = max(avg - balance, 0) + epsilon
    let mut weights_f64 = Vec::with_capacity(balances.len());
    for b in &balances {
        let need = (avg - *b).max(Decimal::ZERO) + epsilon;
        // clamp pour éviter NaN/inf au cast
        let w = (need.to_f64().unwrap_or(0.0)).max(0.0);
        weights_f64.push(w);
    }

    // Cas dégénéré: si tout égal et epsilon tout petit,
    // WeightedIndex gère tant que somme > 0.
    let dist = WeightedIndex::new(&weights_f64).map_err(|_| anyhow::anyhow!("poids invalides"))?;
    let mut rng = thread_rng();
    let idx = dist.sample(&mut rng);

    Ok(admin_addrs[idx].clone())
}

/// Version “clé de chiffrement” X25519: retourne la X25519 hex de l’admin choisi.
pub async fn pick_admin_xpk_weighted<F, Fut>(
    admin_addrs: &[String],
    fetch_balance: F,
    epsilon: Decimal,
) -> Result<String>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<Decimal>>,
{
    let addr = pick_admin_address_weighted(admin_addrs, fetch_balance, epsilon).await?;
    let (_, xpk) =
        decode_address(&addr).map_err(|e| anyhow::anyhow!("adresse admin invalide: {e}"))?;
    Ok(xpk)
}

/// Helper MVP pour choisir une clé publique X25519 admin.
/// Pour l’instant, les balances sont toujours 0.
/// TODO: brancher sur l’index UTXO ou ton store pour le vrai solde.
pub async fn pick_admin_recipient(admin_wallets: &[String]) -> Result<String> {
    let epsilon = Decimal::new(1, 3); // 0.001

    // Stub MVP: toujours 0. Remplace par un vrai fetch UTXO.
    let fetch_balance = |_: &str| async move { Ok(Decimal::ZERO) };

    pick_admin_xpk_weighted(admin_wallets, fetch_balance, epsilon).await
}

pub fn pick_index_from_balances<R: Rng + ?Sized>(
    balances: &[Decimal],
    epsilon: Decimal,
    rng: &mut R,
) -> anyhow::Result<usize> {
    if balances.is_empty() {
        anyhow::bail!("balances is empty");
    }

    // Moyenne
    let sum: Decimal = balances.iter().copied().sum();
    let n = Decimal::from(balances.len() as u64);
    let avg = if n.is_zero() { Decimal::ZERO } else { sum / n };

    // Poids = (avg - balance).max(0) + epsilon
    let mut weights = Vec::with_capacity(balances.len());
    for b in balances {
        let need = (avg - *b).max(Decimal::ZERO) + epsilon;
        let w = need.to_f64().unwrap_or(0.0).max(0.0);
        weights.push(w);
    }

    let dist = WeightedIndex::new(&weights)?;
    Ok(dist.sample(rng))
}

pub async fn pick_admin_wallet_weighted(
    admins: &[Wallet],
    store: &RocksStore,
    hrp: &str,
    scan_limit: usize,
    epsilon: Decimal,
) -> anyhow::Result<Wallet> {
    if admins.is_empty() {
        anyhow::bail!("admin wallet list is empty");
    }

    // 1) lire les soldes réels des wallets
    let mut balances = Vec::with_capacity(admins.len());
    for w in admins {
        let b = w.balance(store, hrp, scan_limit).await?;
        balances.push(b.max(Decimal::ZERO));
    }

    // 2) utiliser la fonction pure
    let mut rng = thread_rng();
    let idx = pick_index_from_balances(&balances, epsilon, &mut rng)?;
    Ok(admins[idx].clone())
}
