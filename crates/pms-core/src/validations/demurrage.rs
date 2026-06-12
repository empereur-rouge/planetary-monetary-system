//! Demurrage natif opt-in par asset (protocole 2.5).
//!
//! Un asset dont `TokenMetadata.demurrage_bps_per_day` est défini voit ses
//! UTXOs perdre de la valeur avec le temps : la valeur **effective** d'un
//! UTXO à la dépense est calculée à la lecture (jamais bloc par bloc, comme
//! des intérêts) :
//!
//! ```text
//! jours    = floor((now_ms - created_at) / 86_400_000)   // jours PLEINS
//! décote   = amount × bps × jours / 10_000
//! effective = max(amount - décote, 0)
//! ```
//!
//! La granularité en jours pleins rend le calcul reproductible côté client :
//! une transaction construite et validée dans la même journée UTC-relative
//! produit la même valeur effective des deux côtés.
//!
//! # Conservation
//!
//! Pour un asset à demurrage, la règle de conservation devient
//! `sum(outputs) <= sum(effective_inputs)` — l'écart est la décote, brûlée
//! implicitement (la supply circulante diminue d'autant : le spend retire le
//! nominal du supply cache, les outputs ne ré-injectent que l'effectif).
//! Pour tous les autres assets, la conservation STRICTE (audit M-7) reste
//! inchangée.
//!
//! # Source de `created_at`
//!
//! `TxOutput::created_at` est estampillé PAR LE SYSTÈME au persist du bloc
//! (anti-antidatage) ; `None` (UTXO pré-v0.10.0) = aucune décote ne court.

use rust_decimal::Decimal;

/// Millisecondes par jour (base du demurrage).
pub const DAY_MS: u64 = 86_400_000;

/// Nombre de jours PLEINS écoulés entre `created_at_ms` et `now_ms`.
/// Horloge en arrière (now < created) → 0.
pub fn elapsed_full_days(created_at_ms: u64, now_ms: u64) -> u64 {
    now_ms.saturating_sub(created_at_ms) / DAY_MS
}

/// Valeur effective d'un UTXO d'un asset à demurrage, à l'instant `now_ms`.
///
/// - `created_at = None` (UTXO pré-upgrade) → valeur nominale intacte.
/// - `bps_per_day = 0` → valeur nominale intacte.
/// - Décote linéaire par jour plein, plancher à zéro (jamais négatif).
///
/// # Examples
///
/// ```
/// use pms_core::validations::demurrage::{DAY_MS, effective_value};
/// use rust_decimal::Decimal;
///
/// let amount = Decimal::from(1000);
/// // 100 bps/jour = 1%/jour. Après 3 jours pleins : 1000 - 30 = 970.
/// let v = effective_value(amount, Some(0), 3 * DAY_MS, 100);
/// assert_eq!(v, Decimal::from(970));
/// // Même jour (0 jour plein) : pas de décote.
/// assert_eq!(effective_value(amount, Some(0), DAY_MS - 1, 100), amount);
/// ```
pub fn effective_value(
    amount: Decimal,
    created_at: Option<u64>,
    now_ms: u64,
    bps_per_day: u32,
) -> Decimal {
    let Some(created) = created_at else {
        return amount;
    };
    if bps_per_day == 0 {
        return amount;
    }
    let days = elapsed_full_days(created, now_ms);
    if days == 0 {
        return amount;
    }
    // décote = amount × bps × days / 10_000 — Decimal exact, pas de float.
    let decay = amount * Decimal::from(bps_per_day) * Decimal::from(days) / Decimal::from(10_000);
    (amount - decay).max(Decimal::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_decay_without_created_at_or_rate() {
        let a = Decimal::from(500);
        println!("none created_at: {}", effective_value(a, None, 10 * DAY_MS, 100));
        assert_eq!(effective_value(a, None, 10 * DAY_MS, 100), a);
        println!("zero rate: {}", effective_value(a, Some(0), 10 * DAY_MS, 0));
        assert_eq!(effective_value(a, Some(0), 10 * DAY_MS, 0), a);
    }

    #[test]
    fn decay_is_floored_at_zero() {
        // 1000 bps/jour = 10%/jour → après 20 jours, décote 200% → plancher 0.
        let v = effective_value(Decimal::from(100), Some(0), 20 * DAY_MS, 1000);
        println!("over-decayed: {v}");
        assert_eq!(v, Decimal::ZERO);
    }

    #[test]
    fn partial_day_does_not_decay() {
        let a = Decimal::from(100);
        let v = effective_value(a, Some(1000), 1000 + DAY_MS - 1, 500);
        println!("23h59 elapsed: {v}");
        assert_eq!(v, a, "moins d'un jour plein = pas de décote");
    }

    #[test]
    fn clock_backwards_is_safe() {
        let a = Decimal::from(100);
        let v = effective_value(a, Some(DAY_MS * 5), DAY_MS, 500);
        println!("clock backwards: {v}");
        assert_eq!(v, a);
    }
}
