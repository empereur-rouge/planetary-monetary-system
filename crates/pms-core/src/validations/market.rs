//! Consensus validation for atomic marketplace settlements (protocole 2.7).
//!
//! A [`pms_types::PlainPayload::MarketSettle`] block declares a sale — item
//! (`asset_sold`, `quantity`), payment (`price` in `price_asset`), and the two
//! parties (`seller`, `buyer`) — and wraps a co-signed UTXO transaction that
//! actually moves the funds. This module enforces, **at the validator (not the
//! build path)**, that the wrapped transaction:
//!
//!   1. moves `quantity` of `asset_sold` from `seller` to `buyer` (item leg),
//!   2. makes `buyer` pay the full `price` in `price_asset` (payment leg),
//!   3. pays the resale royalty (`price × royalty_bps / 10_000`, floored to the
//!      price asset's decimals) to the asset's royalty beneficiary,
//!   4. pays the remainder (`price − royalty`) to the `seller`.
//!
//! Any settlement whose wrapped tx does not satisfy all four is rejected. The
//! royalty policy (`royalty_bps` + beneficiary) is resolved from the registry
//! entry of `asset_sold` ([`pms_types::TokenMetadata::effective_royalty`]) —
//! works for any token OR SFT class, on any ledger, with the royalty paid in the
//! **payment asset** (not necessarily native PMS).
//!
//! The UTXO mechanics (signatures, per-input ownership, per-asset conservation,
//! compliance) are validated separately by the shared `validate_plain_txutxo`
//! path — this module only adds the settlement-shape + role-binding gate on top.

use crate::validations::ownership::address_identity;
use pms_types::TxOutput;
use rust_decimal::{Decimal, RoundingStrategy};
use std::collections::HashMap;

/// Native PMS precision (satoshi-like, 8 decimals). Used to floor the royalty
/// when the price asset is native PMS (no registry entry to read `decimals`
/// from). Custom assets use their registered `decimals`.
pub const PMS_NATIVE_DECIMALS: u32 = 8;

/// Resolves the decimal precision used to floor a royalty for a given price
/// asset. `None` (or an unregistered asset ⇒ `meta_decimals == None`) falls back
/// to [`PMS_NATIVE_DECIMALS`]. **Shared by the server builder and the consensus
/// validator so both floor at the identical precision.**
pub fn price_decimals(meta_decimals: Option<u8>) -> u32 {
    meta_decimals.map(u32::from).unwrap_or(PMS_NATIVE_DECIMALS)
}

/// Upper bound on a declared settlement `price`/`quantity`. Prevents the
/// `price × bps` multiplication in [`compute_royalty`] from overflowing
/// `Decimal` (audit F3) and rejects absurd declarations early. `1e18` is far
/// above any realistic price yet leaves ~10 orders of magnitude of headroom
/// before `Decimal::MAX` (~7.9e28) even at `bps = 10_000`.
pub const MAX_SETTLEMENT_AMOUNT: i128 = 1_000_000_000_000_000_000; // 1e18

/// Computes the resale royalty: `price × royalty_bps / 10_000`, rounded **down**
/// (`ToZero`) to `decimals`. Flooring guarantees the beneficiary is never
/// credited beyond the exact policy share and the seller absorbs any sub-unit
/// dust (`price − royalty` stays exact and non-negative). Returns `Some(0)` when
/// `royalty_bps == 0`, and **`None` on arithmetic overflow** (audit F3 — a
/// consensus validator must never panic on attacker-influenced input).
///
/// This is the **single source of truth** shared verbatim by the server builder
/// (`/v1/market/settle`) and the consensus validator — they cannot diverge.
pub fn compute_royalty(price: Decimal, royalty_bps: u32, decimals: u32) -> Option<Decimal> {
    if royalty_bps == 0 {
        return Some(Decimal::ZERO);
    }
    let raw = price
        .checked_mul(Decimal::from(royalty_bps))?
        .checked_div(Decimal::from(10_000u32))?;
    Some(raw.round_dp_with_strategy(decimals, RoundingStrategy::ToZero))
}

/// Declared settlement parameters + resolved royalty, ready for the shape check.
///
/// `royalty`/`beneficiary` are the already-resolved output of the asset's
/// royalty policy (via [`compute_royalty`] + `effective_royalty`). `beneficiary`
/// is `None` when the asset carries no royalty policy; it may be `Some` with
/// `royalty == 0` when the computed share floors to zero (tiny price / coarse
/// decimals). `validate_settlement` only reads `beneficiary` when `royalty > 0`.
#[derive(Debug, Clone)]
pub struct SettlementCheck<'a> {
    /// Item asset id (token asset_id or SFT `"collection:class"`). Never native.
    pub asset_sold: &'a str,
    /// Quantity of the item transferred to the buyer.
    pub quantity: Decimal,
    /// Payment asset: `None` = native PMS, `Some(id)` = token/SFT.
    pub price_asset: Option<&'a str>,
    /// Total price paid by the buyer (before the royalty split).
    pub price: Decimal,
    /// Declared seller (current owner of the item).
    pub seller: &'a str,
    /// Declared buyer (receives the item).
    pub buyer: &'a str,
    /// Royalty amount owed to `beneficiary`, in `price_asset` (0 = none).
    pub royalty: Decimal,
    /// Royalty beneficiary; `None` iff `royalty == 0`.
    pub beneficiary: Option<&'a str>,
}

/// Validates that the wrapped transaction satisfies the declared settlement.
///
/// * `input_outputs` — the resolved UTXOs the tx spends, each carrying its owner
///   `address`, `asset_id` and `amount` (from `validate_plain_txutxo`).
/// * `outputs` — the tx outputs.
///
/// Returns `Err(reason)` on any violation (reason is a stable, non-sensitive
/// string suitable for `PutResult::Rejected`).
///
/// # Exact-accounting invariant (audit F1)
///
/// The check is expressed on **net balance change per `(address, asset)`**
/// (`Σ outputs − Σ inputs`). Every economic constraint is an **equality**, not a
/// floor — otherwise a forged settlement could declare a tiny `price` while the
/// wrapped tx routes the real value to the seller, evading the royalty. The rule:
///
///   * the **only** addresses allowed to *net-gain* any asset are
///     `buyer` (exactly `quantity` of `asset_sold`), `seller` (exactly
///     `net_to_seller` of `price_asset`) and `beneficiary` (exactly `royalty` of
///     `price_asset`) — accumulated per `(address, asset)` so `beneficiary ==
///     seller` sums to `price`;
///   * ANY other positive net — a different amount, a different asset (a
///     side-channel payment), or a third-party recipient — is **rejected**;
///   * the `seller` must net-*lose* exactly `quantity` of `asset_sold`.
///
/// Together with per-asset conservation (checked upstream by
/// `validate_plain_txutxo`), this pins the value flowing to the seller and the
/// creator to exactly the declared price split — the buyer therefore pays at
/// least `price`, and the royalty is levied on the true amount moved.
pub fn validate_settlement(
    input_outputs: &[TxOutput],
    outputs: &[TxOutput],
    c: &SettlementCheck<'_>,
) -> Result<(), String> {
    // ── Structural sanity ────────────────────────────────────────────────
    let max = Decimal::from(MAX_SETTLEMENT_AMOUNT);
    if c.quantity <= Decimal::ZERO || c.quantity > max {
        return Err("settlement: quantity out of range".into());
    }
    if c.price <= Decimal::ZERO || c.price > max {
        return Err("settlement: price out of range".into());
    }
    if c.asset_sold.trim().is_empty() {
        return Err("settlement: asset_sold required".into());
    }
    if c.seller.trim().is_empty() || c.buyer.trim().is_empty() {
        return Err("settlement: seller and buyer required".into());
    }
    if c.seller == c.buyer {
        return Err("settlement: buyer and seller must differ".into());
    }
    // A sale is item ↔ payment: the two legs must be distinct assets.
    if c.price_asset == Some(c.asset_sold) {
        return Err("settlement: asset_sold and price_asset must differ".into());
    }
    if c.royalty < Decimal::ZERO || c.royalty > c.price {
        return Err("settlement: royalty out of range".into());
    }
    if c.royalty > Decimal::ZERO && c.beneficiary.is_none() {
        return Err("settlement: royalty > 0 requires a beneficiary".into());
    }
    // audit A2: the royalty beneficiary cannot be the buyer — they would both pay
    // the price AND receive their own royalty, netting negative in `price_asset`,
    // which the exact-gain accounting below can't express (it would fail with a
    // confusing "must net-receive"). Reject early with a clear reason. (The
    // `beneficiary == seller` case — a creator reselling their own item — IS
    // supported: the gains accumulate to `price`.)
    if c.royalty > Decimal::ZERO && c.beneficiary == Some(c.buyer) {
        return Err("settlement: royalty beneficiary cannot be the buyer".into());
    }
    let net_to_seller = c.price - c.royalty;
    let item = Some(c.asset_sold);

    // Identités des parties déclarées : les inputs/outputs sont attribués par
    // IDENTITÉ (cf. `address_identity`), pas par string brute, pour qu'un UTXO
    // détenu sous la pubkey hex (forme SDK) soit correctement compté comme
    // appartenant au `seller`/`buyer` déclaré en bech32m (régression #1a —
    // sinon un item minté en hex est invendable, « fonds piégés »).
    let seller_id = address_identity(c.seller);
    let buyer_id = address_identity(c.buyer);

    // ── Net change per (identity, asset): + received, − spent ────────────
    // Checked arithmetic (audit A3): a consensus validator must never panic on
    // attacker-influenced amounts — `Decimal`'s `+=`/`-=` panic on overflow.
    let mut net: HashMap<(String, Option<&str>), Decimal> = HashMap::new();
    for o in outputs {
        let a = Decimal::from_str_exact(&o.amount)
            .map_err(|_| format!("settlement: bad output amount {}", o.amount))?;
        let slot = net
            .entry((address_identity(&o.address), o.asset_id.as_deref()))
            .or_default();
        *slot = slot
            .checked_add(a)
            .ok_or_else(|| "settlement: amount overflow".to_string())?;
    }
    for i in input_outputs {
        let a = Decimal::from_str_exact(&i.amount)
            .map_err(|_| format!("settlement: bad input amount {}", i.amount))?;
        let slot = net
            .entry((address_identity(&i.address), i.asset_id.as_deref()))
            .or_default();
        *slot = slot
            .checked_sub(a)
            .ok_or_else(|| "settlement: amount overflow".to_string())?;
    }
    let get = |ident: &str, asset: Option<&str>| -> Decimal {
        net.get(&(ident.to_string(), asset)).copied().unwrap_or(Decimal::ZERO)
    };

    // ── The EXHAUSTIVE set of allowed positive nets (audit F1) ───────────
    // Accumulated per (identity, asset) so beneficiary == seller sums to `price`.
    let mut allowed: HashMap<(String, Option<&str>), Decimal> = HashMap::new();
    *allowed.entry((buyer_id, item)).or_default() += c.quantity;
    *allowed.entry((seller_id.clone(), c.price_asset)).or_default() += net_to_seller;
    if c.royalty > Decimal::ZERO {
        let b = c.beneficiary.expect("beneficiary present when royalty > 0");
        *allowed.entry((address_identity(b), c.price_asset)).or_default() += c.royalty;
    }

    // 1. Every positive net MUST be a declared gain of the EXACT expected amount.
    //    Blocks: over-payment to seller (price under-declaration), side-asset
    //    payments, third-party recipients, and wrong amounts.
    for ((ident, asset), n) in &net {
        if *n > Decimal::ZERO {
            match allowed.get(&(ident.clone(), *asset)) {
                Some(exp) if exp == n => {}
                _ => {
                    return Err(format!(
                        "settlement: unexpected credit {} {} to {} (not part of the declared split)",
                        n,
                        asset.unwrap_or("PMS"),
                        ident
                    ));
                }
            }
        }
    }
    // 2. Every declared gain MUST actually be paid in full (no missing/short output).
    for ((ident, asset), exp) in &allowed {
        if *exp > Decimal::ZERO && get(ident, *asset) != *exp {
            return Err(format!(
                "settlement: {} must net-receive exactly {} of {}",
                ident,
                exp,
                asset.unwrap_or("PMS")
            ));
        }
    }
    // 3. Seller relinquishes EXACTLY `quantity` of the item.
    if get(&seller_id, item) != -c.quantity {
        return Err(format!(
            "settlement: seller must net-relinquish exactly {} of {}",
            c.quantity, c.asset_sold
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(addr: &str, amount: &str, asset: Option<&str>) -> TxOutput {
        TxOutput::new(addr, amount, asset.map(str::to_string))
    }
    fn d(s: &str) -> Decimal {
        Decimal::from_str_exact(s).unwrap()
    }

    // Baseline: Alice sells 1 "studio:ticket" to Bob for 100 usdc, 20% royalty
    // to the creator. Payment asset = custom token "usdc". Gas paid in PMS.
    fn baseline_check<'a>() -> SettlementCheck<'a> {
        SettlementCheck {
            asset_sold: "studio:ticket",
            quantity: d("1"),
            price_asset: Some("usdc"),
            price: d("100"),
            seller: "alice",
            buyer: "bob",
            royalty: d("20"),
            beneficiary: Some("creator"),
        }
    }

    #[test]
    fn compute_royalty_floors_to_decimals() {
        // 100 * 2000 / 10000 = 20 exactly.
        assert_eq!(compute_royalty(d("100"), 2000, 8), Some(d("20")));
        // 33.333333 * 1500 / 10000 = 4.99999995 → floored to 2dp = 4.99
        assert_eq!(compute_royalty(d("33.333333"), 1500, 2), Some(d("4.99")));
        // 0 bps → 0
        assert_eq!(compute_royalty(d("100"), 0, 8), Some(d("0")));
        println!("compute_royalty floors correctly: OK");
    }

    #[test]
    fn compute_royalty_overflow_returns_none_not_panic() {
        // audit F3: price near Decimal::MAX must not panic the validator.
        let huge = Decimal::MAX;
        assert_eq!(compute_royalty(huge, 10_000, 8), None, "overflow → None, no panic");
        println!("compute_royalty overflow → None: OK");
    }

    #[test]
    fn reject_price_underdeclaration_moves_real_value() {
        // audit F1: declare a tiny price (royalty rounds to 0) but route 100 usdc
        // to the seller. Exact accounting must reject: seller nets 100 ≠ 0.0001.
        let mut c = baseline_check();
        c.price = d("0.0001");
        c.royalty = d("0"); // floors to 0 at this price
        c.beneficiary = None;
        let inputs = vec![out("bob", "100", Some("usdc")), out("alice", "1", Some("studio:ticket"))];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),
            out("alice", "100", Some("usdc")), // seller grabs the real 100
        ];
        let r = validate_settlement(&inputs, &outputs, &c);
        println!("price under-declaration (0.0001 declared, 100 moved) → {r:?}");
        assert!(r.is_err(), "under-declared price must be rejected");
        assert!(r.unwrap_err().contains("unexpected credit"));
    }

    #[test]
    fn reject_side_asset_payment_to_seller() {
        // audit F1: pay the declared price in a worthless asset, route real value
        // to the seller in a DIFFERENT asset. The side credit must be rejected.
        let inputs = vec![
            out("bob", "100", Some("usdc")),          // declared payment
            out("bob", "100", Some("realtoken")),     // side value
            out("alice", "1", Some("studio:ticket")),
        ];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),
            out("creator", "20", Some("usdc")),
            out("alice", "80", Some("usdc")),
            out("alice", "100", Some("realtoken")), // seller paid in a side asset
        ];
        let r = validate_settlement(&inputs, &outputs, &baseline_check());
        println!("side-asset payment to seller → {r:?}");
        assert!(r.is_err(), "side-asset payment must be rejected");
        assert!(r.unwrap_err().contains("unexpected credit"));
    }

    #[test]
    fn reject_seller_overpaid() {
        // Seller gets 85 (>80). Exact accounting rejects (not a floor).
        let inputs = vec![out("bob", "105", Some("usdc")), out("alice", "1", Some("studio:ticket"))];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),
            out("creator", "20", Some("usdc")),
            out("alice", "85", Some("usdc")), // over-paid seller
        ];
        let r = validate_settlement(&inputs, &outputs, &baseline_check());
        println!("seller over-paid (85>80) → {r:?}");
        assert!(r.is_err(), "over-payment to seller must be rejected");
    }

    #[test]
    fn reject_third_party_credit_in_price_asset() {
        let inputs = vec![out("bob", "120", Some("usdc")), out("alice", "1", Some("studio:ticket"))];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),
            out("creator", "20", Some("usdc")),
            out("alice", "80", Some("usdc")),
            out("mallory", "20", Some("usdc")), // unaccounted third-party credit
        ];
        let r = validate_settlement(&inputs, &outputs, &baseline_check());
        println!("third-party credit → {r:?}");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("unexpected credit"));
    }

    #[test]
    fn happy_custom_token_price() {
        // Bob spends 100 usdc; Alice spends 1 ticket.
        let inputs = vec![out("bob", "100", Some("usdc")), out("alice", "1", Some("studio:ticket"))];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),   // item to buyer
            out("creator", "20", Some("usdc")),        // royalty
            out("alice", "80", Some("usdc")),          // seller net
        ];
        let c = baseline_check();
        let r = validate_settlement(&inputs, &outputs, &c);
        println!("happy custom-token settlement → {r:?}");
        assert!(r.is_ok(), "{r:?}");
    }

    #[test]
    fn happy_native_pms_price_with_change_and_gas() {
        // Price asset = native PMS. Bob has a 130 PMS UTXO; 100 price + ~2 gas
        // burned + 28 change. Alice provides the ticket.
        let c = SettlementCheck { price_asset: None, ..baseline_check() };
        let inputs = vec![out("bob", "130", None), out("alice", "1", Some("studio:ticket"))];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),
            out("creator", "20", None),
            out("alice", "80", None),
            out("bob", "28", None), // change (fee 2 burned: in 130 = out 129 → 1 burned; use 28 change so 1 burned)
        ];
        // 130 in; out = 1(item, different asset) + 20 + 80 + 28 = 128 PMS → 2 burned. Fine.
        let r = validate_settlement(&inputs, &outputs, &c);
        println!("happy native-PMS settlement → {r:?}");
        assert!(r.is_ok(), "{r:?}");
    }

    #[test]
    fn happy_item_change_partial_sell() {
        // Alice holds a single 5-unit UTXO of the class, sells 1, keeps 4.
        let inputs = vec![out("bob", "100", Some("usdc")), out("alice", "5", Some("studio:ticket"))];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),
            out("alice", "4", Some("studio:ticket")), // item change
            out("creator", "20", Some("usdc")),
            out("alice", "80", Some("usdc")),
        ];
        let r = validate_settlement(&inputs, &outputs, &baseline_check());
        println!("partial-sell (item change) → {r:?}");
        assert!(r.is_ok(), "{r:?}");
    }

    #[test]
    fn reject_royalty_too_low() {
        // Creator gets only 10 instead of 20 → seller would get 90 (over net).
        let inputs = vec![out("bob", "100", Some("usdc")), out("alice", "1", Some("studio:ticket"))];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),
            out("creator", "10", Some("usdc")), // TAMPERED
            out("alice", "90", Some("usdc")),
        ];
        let r = validate_settlement(&inputs, &outputs, &baseline_check());
        println!("tampered low royalty → {r:?}");
        assert!(r.is_err(), "under-paid royalty must be rejected");
        assert!(r.unwrap_err().contains("settlement"), "settlement-shape violation");
    }

    #[test]
    fn reject_missing_royalty_output() {
        let inputs = vec![out("bob", "100", Some("usdc")), out("alice", "1", Some("studio:ticket"))];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),
            out("alice", "100", Some("usdc")), // seller takes everything, no royalty
        ];
        let r = validate_settlement(&inputs, &outputs, &baseline_check());
        println!("missing royalty → {r:?}");
        assert!(r.is_err(), "missing royalty must be rejected");
    }

    #[test]
    fn reject_buyer_not_receiving_item() {
        let inputs = vec![out("bob", "100", Some("usdc")), out("alice", "1", Some("studio:ticket"))];
        let outputs = vec![
            // item wrongly sent to a third party
            out("mallory", "1", Some("studio:ticket")),
            out("creator", "20", Some("usdc")),
            out("alice", "80", Some("usdc")),
        ];
        let r = validate_settlement(&inputs, &outputs, &baseline_check());
        println!("buyer not receiving item → {r:?}");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("settlement"));
    }

    #[test]
    fn reject_wrong_quantity() {
        let mut c = baseline_check();
        c.quantity = d("2"); // declares 2 but tx only moves 1
        let inputs = vec![out("bob", "100", Some("usdc")), out("alice", "1", Some("studio:ticket"))];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),
            out("creator", "20", Some("usdc")),
            out("alice", "80", Some("usdc")),
        ];
        let r = validate_settlement(&inputs, &outputs, &c);
        println!("declared qty 2 but moved 1 → {r:?}");
        assert!(r.is_err());
    }

    #[test]
    fn reject_buyer_underpays() {
        // Buyer only funds 50 usdc but outputs claim 100 distributed — conservation
        // is checked elsewhere; here the buyer-pays-price binding must catch it.
        let inputs = vec![out("bob", "50", Some("usdc")), out("alice", "1", Some("studio:ticket"))];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),
            out("creator", "20", Some("usdc")),
            out("alice", "30", Some("usdc")), // only 50 total out
        ];
        let mut c = baseline_check();
        c.royalty = d("20");
        // seller net = 30 < net_to_seller 80 → rejected anyway; also buyer net = -50 > -100.
        let r = validate_settlement(&inputs, &outputs, &c);
        println!("buyer underpays → {r:?}");
        assert!(r.is_err());
    }

    #[test]
    fn reject_role_swap_seller_provides_payment() {
        // Alice (seller) tries to also fund the payment; buyer provides nothing.
        let inputs = vec![out("alice", "100", Some("usdc")), out("alice", "1", Some("studio:ticket"))];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),
            out("creator", "20", Some("usdc")),
            out("alice", "80", Some("usdc")),
        ];
        let r = validate_settlement(&inputs, &outputs, &baseline_check());
        println!("seller funds payment (buyer pays nothing) → {r:?}");
        assert!(r.is_err(), "buyer-pays binding must reject");
    }

    #[test]
    fn happy_beneficiary_equals_seller() {
        // Creator resells their own item: beneficiary == seller. Required gains
        // must SUM to price (80 + 20 = 100), not max.
        let mut c = baseline_check();
        c.seller = "creator";
        c.beneficiary = Some("creator");
        let inputs = vec![out("bob", "100", Some("usdc")), out("creator", "1", Some("studio:ticket"))];
        let outputs = vec![
            out("bob", "1", Some("studio:ticket")),
            out("creator", "100", Some("usdc")), // net 80 + royalty 20 combined
        ];
        let r = validate_settlement(&inputs, &outputs, &c);
        println!("beneficiary==seller (combined 100) → {r:?}");
        assert!(r.is_ok(), "{r:?}");

        // And if creator only takes 90, it must be rejected (needs full 100).
        let outputs_bad = vec![
            out("bob", "1", Some("studio:ticket")),
            out("creator", "90", Some("usdc")),
        ];
        let r2 = validate_settlement(&inputs, &outputs_bad, &c);
        println!("beneficiary==seller short (90<100) → {r2:?}");
        assert!(r2.is_err());
    }

    #[test]
    fn reject_same_asset_both_legs() {
        let mut c = baseline_check();
        c.price_asset = Some("studio:ticket"); // item == payment
        let inputs = vec![out("bob", "100", Some("studio:ticket")), out("alice", "1", Some("studio:ticket"))];
        let outputs = vec![out("bob", "1", Some("studio:ticket"))];
        let r = validate_settlement(&inputs, &outputs, &c);
        println!("same asset both legs → {r:?}");
        assert!(r.is_err());
    }

    #[test]
    fn reject_buyer_equals_seller() {
        let mut c = baseline_check();
        c.buyer = "alice"; // == seller
        let r = validate_settlement(&[], &[], &c);
        println!("buyer==seller → {r:?}");
        assert!(r.is_err());
    }

    #[test]
    fn reject_beneficiary_equals_buyer() {
        // audit A2: the royalty beneficiary cannot be the buyer — clear reason,
        // not a confusing "must net-receive".
        let mut c = baseline_check();
        c.beneficiary = Some("bob"); // == buyer
        let r = validate_settlement(&[], &[], &c);
        println!("beneficiary==buyer → {r:?}");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("beneficiary cannot be the buyer"));
    }

    // ── Régression #1a : identité d'adresse (formes hex ↔ bech32m) ──────────

    use pms_wallet::{SignerBackend, Wallet};

    /// L'identité collapse la pubkey hex brute (forme SDK), sa variante `0x`/casse,
    /// et le bech32m d'un MÊME wallet vers le même hash20 ; deux wallets diffèrent.
    #[test]
    fn address_identity_collapses_wallet_forms() {
        let w = Wallet::from_seed(&[9u8; 32], None).expect("wallet");
        let hex = w.public_key_hex.clone();
        let bech32 = w.get_address("8e");
        let (h20, _x) = pms_wallet::decode_address(&bech32).expect("decode");

        let id_hex = address_identity(&hex);
        let id_bech32 = address_identity(&bech32);
        let id_0x_upper = address_identity(&format!("0x{}", hex.to_uppercase()));
        println!("id(hex)={id_hex}\nid(bech32)={id_bech32}\nid(0xUPPER)={id_0x_upper}\nhash20={h20}");

        assert_eq!(id_hex, h20, "identité(hex) == hash20 embarqué dans le bech32m");
        assert_eq!(id_bech32, h20, "identité(bech32m) == son hash20");
        assert_eq!(id_0x_upper, h20, "0x + casse normalisés vers la même identité");

        let other = Wallet::from_seed(&[10u8; 32], None).expect("wallet");
        assert_ne!(
            address_identity(&other.public_key_hex),
            id_hex,
            "deux wallets distincts ont des identités distinctes"
        );
    }

    /// **Cœur de la régression #1a** : l'item du vendeur est détenu sous sa
    /// pubkey HEX (forme SDK), mais le settlement le déclare en BECH32m. Le
    /// validateur DOIT l'accepter (avant le fix : « seller must net-relinquish »).
    #[test]
    fn accept_seller_item_input_under_hex_form() {
        let seller = Wallet::from_seed(&[11u8; 32], None).expect("wallet");
        let buyer = Wallet::from_seed(&[12u8; 32], None).expect("wallet");
        let seller_bech32 = seller.get_address("8e");
        let seller_hex = seller.public_key_hex.clone(); // forme SDK
        let buyer_bech32 = buyer.get_address("8e");

        // Revente 100 usdc, aucune royalty (swap pur).
        let c = SettlementCheck {
            asset_sold: "studio:ticket",
            quantity: d("1"),
            price_asset: Some("usdc"),
            price: d("100"),
            seller: &seller_bech32,
            buyer: &buyer_bech32,
            royalty: d("0"),
            beneficiary: None,
        };
        // Item détenu SOUS LA FORME HEX ; paiement acheteur en bech32m.
        let inputs = vec![
            out(&buyer_bech32, "100", Some("usdc")),
            out(&seller_hex, "1", Some("studio:ticket")),
        ];
        let outputs = vec![
            out(&buyer_bech32, "1", Some("studio:ticket")), // item → acheteur
            out(&seller_bech32, "100", Some("usdc")),       // net → vendeur (forme canonique)
        ];
        let r = validate_settlement(&inputs, &outputs, &c);
        println!("seller item under hex-form, declared bech32m → {r:?}");
        assert!(r.is_ok(), "l'item hex du vendeur doit être vendable: {r:?}");
    }

    /// Symétrique : l'acheteur PAIE depuis un UTXO détenu sous sa forme HEX,
    /// avec change vers sa forme bech32m. Les deux formes collapsent → Ok.
    #[test]
    fn accept_buyer_payment_input_under_hex_form() {
        let seller = Wallet::from_seed(&[13u8; 32], None).expect("wallet");
        let buyer = Wallet::from_seed(&[14u8; 32], None).expect("wallet");
        let seller_bech32 = seller.get_address("8e");
        let buyer_bech32 = buyer.get_address("8e");
        let buyer_hex = buyer.public_key_hex.clone();

        let c = SettlementCheck {
            asset_sold: "studio:ticket",
            quantity: d("1"),
            price_asset: Some("usdc"),
            price: d("100"),
            seller: &seller_bech32,
            buyer: &buyer_bech32,
            royalty: d("0"),
            beneficiary: None,
        };
        // Acheteur paie 120 usdc DEPUIS SA FORME HEX, 20 de change vers bech32m.
        let inputs = vec![
            out(&buyer_hex, "120", Some("usdc")),
            out(&seller_bech32, "1", Some("studio:ticket")),
        ];
        let outputs = vec![
            out(&buyer_bech32, "1", Some("studio:ticket")),
            out(&seller_bech32, "100", Some("usdc")),
            out(&buyer_bech32, "20", Some("usdc")), // change acheteur (autre forme)
        ];
        let r = validate_settlement(&inputs, &outputs, &c);
        println!("buyer pays from hex-form, change to bech32m → {r:?}");
        assert!(r.is_ok(), "le paiement hex de l'acheteur doit être accepté: {r:?}");
    }

    /// Garde-fou : la normalisation NE doit PAS accepter un input détenu par un
    /// TIERS comme relinquish du vendeur. Un item sous une pubkey hex ÉTRANGÈRE
    /// (identité ≠ vendeur déclaré) → rejet « net-relinquish ».
    #[test]
    fn reject_foreign_hex_input_as_seller_relinquish() {
        let seller = Wallet::from_seed(&[15u8; 32], None).expect("wallet");
        let buyer = Wallet::from_seed(&[16u8; 32], None).expect("wallet");
        let stranger = Wallet::from_seed(&[17u8; 32], None).expect("wallet");
        let seller_bech32 = seller.get_address("8e");
        let buyer_bech32 = buyer.get_address("8e");
        let stranger_hex = stranger.public_key_hex.clone();

        let c = SettlementCheck {
            asset_sold: "studio:ticket",
            quantity: d("1"),
            price_asset: Some("usdc"),
            price: d("100"),
            seller: &seller_bech32,
            buyer: &buyer_bech32,
            royalty: d("0"),
            beneficiary: None,
        };
        // L'item vient d'un TIERS (hex étranger), pas du vendeur déclaré.
        let inputs = vec![
            out(&buyer_bech32, "100", Some("usdc")),
            out(&stranger_hex, "1", Some("studio:ticket")),
        ];
        let outputs = vec![
            out(&buyer_bech32, "1", Some("studio:ticket")),
            out(&seller_bech32, "100", Some("usdc")),
        ];
        let r = validate_settlement(&inputs, &outputs, &c);
        println!("foreign hex item (not the declared seller) → {r:?}");
        assert!(r.is_err(), "un item d'un tiers ne compte pas comme relinquish du vendeur");
        assert!(r.unwrap_err().contains("net-relinquish"));
    }
}
