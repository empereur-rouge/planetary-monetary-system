use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::ops::{Add, AddAssign, Div, Mul, Sub, SubAssign};
use thiserror::Error;

// ═══════════════════════════════════════════════════════════════════════════════
// Amount - Type monétaire avec précision garantie
// ═══════════════════════════════════════════════════════════════════════════════
//
// Ce type encapsule un Decimal et garantit que toutes les valeurs ont au maximum
// 8 décimales (la précision du token PMS). L'arrondi est automatique après chaque
// opération arithmétique.
//
// Voir Chapitre 19.2 du Rust Book pour les traits opérateurs (Add, Sub, etc.).
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Error)]
pub enum AmountError {
    #[error("format de montant invalide")]
    Parse,
    #[error("montant négatif interdit")]
    Negative,
    #[error("trop de décimales (max {0})")]
    TooManyDecimals(u32),
}

/// Montant monétaire avec précision garantie à 8 décimales.
///
/// # Garanties
/// - Toutes les opérations arithmétiques arrondissent automatiquement le résultat
/// - Impossible d'avoir plus de `DECIMALS` décimales après une opération
///
/// # Exemple
/// ```ignore
/// let a = Amount::parse_pms("10.5")?;
/// let b = Amount::parse_pms("0.03")?;
/// let fee = a * b;  // = 0.315, arrondi si nécessaire
/// println!("{}", fee);  // affiche "0.315"
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Amount(#[serde(with = "rust_decimal::serde::str")] pub Decimal);

impl Amount {
    /// Nombre maximum de décimales pour le token PMS.
    /// Modifie cette valeur ici pour changer la précision globalement.
    pub const DECIMALS: u32 = 8;

    /// Crée un Amount à partir d'un Decimal brut, en arrondissant à DECIMALS.
    ///
    /// # Pourquoi arrondir ?
    /// Quand on multiplie deux Decimals (ex: 0.03549294 * 0.03), le résultat
    /// peut avoir plus de décimales que l'entrée. Cette méthode garantit
    /// que le résultat respecte toujours la précision du protocole.
    #[inline]
    pub fn from_decimal(d: Decimal) -> Self {
        // round_dp = "round to decimal places" - arrondit à N décimales
        Self(d.round_dp(Self::DECIMALS))
    }

    /// Crée un Amount nul (0.00000000)
    #[inline]
    pub fn zero() -> Self {
        Self(Decimal::ZERO)
    }

    /// Parse une chaîne en Amount pour le token PMS (8 décimales max).
    ///
    /// C'est la méthode recommandée pour créer un Amount à partir d'une entrée utilisateur.
    /// Elle vérifie que l'entrée n'a pas trop de décimales ET qu'elle n'est pas négative.
    pub fn parse_pms(s: &str) -> Result<Self, AmountError> {
        Self::parse(s, Self::DECIMALS)
    }

    /// Parse une chaîne en Amount avec un nombre de décimales personnalisé.
    ///
    /// # Arguments
    /// * `s` - La chaîne à parser (ex: "10.5", "0.00000001")
    /// * `max_decimals` - Nombre maximum de décimales autorisées
    ///
    /// # Errors
    /// - `AmountError::Parse` si le format est invalide
    /// - `AmountError::Negative` si le montant est négatif
    /// - `AmountError::TooManyDecimals` si trop de décimales
    pub fn parse(s: &str, max_decimals: u32) -> Result<Self, AmountError> {
        let d = s.parse::<Decimal>().map_err(|_| AmountError::Parse)?;
        if d.is_sign_negative() {
            return Err(AmountError::Negative);
        }
        // scale() retourne le nombre de chiffres après la virgule
        if d.scale() > max_decimals {
            return Err(AmountError::TooManyDecimals(max_decimals));
        }
        Ok(Self(d))
    }

    /// Vérifie si le montant est zéro.
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    /// Retourne le Decimal interne (pour les calculs avancés).
    #[inline]
    pub fn inner(&self) -> Decimal {
        self.0
    }

    /// Soustraction vérifiée : renvoie `None` si le résultat serait négatif.
    ///
    /// # Pourquoi
    /// L'opérateur `Sub` brut produit un `Amount` négatif silencieusement
    /// (ex: `Amount(1) - Amount(5) = Amount(-4)`). Dans un moteur bancaire, une
    /// balance négative non détectée est une sous-couverture (double-spend
    /// implicite). Tout chemin financier qui débite une balance ou paie une fee
    /// DOIT utiliser `checked_sub` et rejeter proprement le cas `None` plutôt
    /// que de laisser le solde passer sous zéro.
    ///
    /// # Exemple
    /// ```
    /// use pms_token::Amount;
    /// let balance = Amount::parse_pms("3").unwrap();
    /// let spend = Amount::parse_pms("5").unwrap();
    /// assert_eq!(balance.checked_sub(spend), None); // solde insuffisant
    /// ```
    #[inline]
    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        let r = self.0 - rhs.0;
        if r.is_sign_negative() {
            None
        } else {
            Some(Self::from_decimal(r))
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Implémentation des opérateurs arithmétiques
// ═══════════════════════════════════════════════════════════════════════════════
//
// Chaque opérateur arrondit automatiquement le résultat à DECIMALS décimales.
// C'est le cœur de la solution au problème des fees à 11 décimales !

/// Addition de deux Amount. Le résultat est arrondi à 8 décimales.
impl Add for Amount {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        // Addition puis arrondi pour garantir la précision
        Self::from_decimal(self.0 + rhs.0)
    }
}

/// Permet d'utiliser += sur un Amount.
impl AddAssign for Amount {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

/// Soustraction de deux Amount. Le résultat est arrondi à 8 décimales.
impl Sub for Amount {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::from_decimal(self.0 - rhs.0)
    }
}

/// Permet d'utiliser -= sur un Amount.
impl SubAssign for Amount {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

/// Multiplication Amount * Amount (ex: montant * ratio).
/// C'est cette opération qui causait le bug des 11 décimales !
impl Mul for Amount {
    type Output = Self;

    fn mul(self, rhs: Self) -> Self::Output {
        // Ex: 0.03549294 * 0.03 = 0.0010647882 → arrondi à 0.00106479
        Self::from_decimal(self.0 * rhs.0)
    }
}

/// Multiplication Amount * Decimal (pour les ratios bruts).
impl Mul<Decimal> for Amount {
    type Output = Self;

    fn mul(self, rhs: Decimal) -> Self::Output {
        Self::from_decimal(self.0 * rhs)
    }
}

/// Division Amount / Amount.
impl Div for Amount {
    type Output = Self;

    fn div(self, rhs: Self) -> Self::Output {
        Self::from_decimal(self.0 / rhs.0)
    }
}

/// Division Amount / Decimal.
impl Div<Decimal> for Amount {
    type Output = Self;

    fn div(self, rhs: Decimal) -> Self::Output {
        Self::from_decimal(self.0 / rhs)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Traits de conversion et affichage
// ═══════════════════════════════════════════════════════════════════════════════

/// Implémentation du trait Display pour Amount.
/// Cela fournit automatiquement la méthode to_string() via le trait ToString
/// qui est implémenté pour tout type implémentant Display.
impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // normalize() supprime les zéros trailing (ex: "1.00" -> "1")
        write!(f, "{}", self.0.normalize())
    }
}

/// Permet de créer un Amount à partir d'un Decimal.
impl From<Decimal> for Amount {
    fn from(d: Decimal) -> Self {
        Self::from_decimal(d)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests unitaires
// ═══════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pms_valid() {
        let a = Amount::parse_pms("10.00000000").unwrap();
        assert_eq!(a.to_string(), "10");
    }

    #[test]
    fn test_parse_pms_too_many_decimals() {
        let e = Amount::parse_pms("1.000000001").unwrap_err();
        // CRITICAL: `matches!(...)` seul (v0.9.2) jetait son bool — ne vérifiait
        // que "une erreur a eu lieu". Encapsulé dans `assert!` pour pinner la variante.
        assert!(
            matches!(e, AmountError::TooManyDecimals(8)),
            "expected TooManyDecimals(8), got {e:?}"
        );
    }

    #[test]
    fn test_from_decimal_rounds() {
        // 0.123456789 a 9 décimales, doit être arrondi à 8
        let d = Decimal::from_str_exact("0.123456789").unwrap();
        let a = Amount::from_decimal(d);
        // Vérifie qu'on a bien 8 décimales max
        assert!(a.0.scale() <= 8);
    }

    #[test]
    fn test_multiplication_rounds() {
        // Simule le calcul de fee: 0.03549294 * 0.03 = 0.0010647882
        let amount = Amount::parse_pms("0.03549294").unwrap();
        let ratio = Amount::parse_pms("0.03").unwrap();
        let fee = amount * ratio;

        // Le résultat doit avoir max 8 décimales
        assert!(
            fee.0.scale() <= 8,
            "Fee has {} decimals, expected <= 8",
            fee.0.scale()
        );

        // Vérifie que la string n'a pas plus de 8 chiffres après la virgule
        let fee_str = fee.to_string();
        if let Some(dot_pos) = fee_str.find('.') {
            let decimals = fee_str.len() - dot_pos - 1;
            assert!(
                decimals <= 8,
                "Fee string '{}' has {} decimals",
                fee_str,
                decimals
            );
        }
    }

    #[test]
    fn test_addition_rounds() {
        let a = Amount::from_decimal(Decimal::from_str_exact("0.123456789").unwrap());
        let b = Amount::from_decimal(Decimal::from_str_exact("0.000000001").unwrap());
        let sum = a + b;
        assert!(sum.0.scale() <= 8);
    }

    #[test]
    fn test_zero() {
        let z = Amount::zero();
        assert!(z.is_zero());
        assert_eq!(z.to_string(), "0");
    }

    // ───────────────────────────────────────────────────────────────────────
    // Edge cases arithmétiques (audit tests v0.9.3 — E5)
    // Comportements MESURÉS de rust_decimal, pinnés pour attraper toute
    // régression de sémantique monétaire.
    // ───────────────────────────────────────────────────────────────────────

    #[test]
    fn raw_sub_below_zero_wraps_to_negative_silently() {
        // CARACTÉRISATION (comportement potentiellement dangereux) : l'opérateur
        // `Sub` ne clampe PAS à zéro — `1 - 5 = -4`. Les balances DOIVENT utiliser
        // `checked_sub`. Ce test échouera si la sémantique de Sub change
        // (saturation, panic), forçant une décision explicite.
        let one = Amount::parse_pms("1").unwrap();
        let five = Amount::parse_pms("5").unwrap();
        let r = one - five;
        println!("raw 1 - 5 = {r}");
        assert!(r.0.is_sign_negative(), "raw Sub currently allows negative");
        assert_eq!(r.to_string(), "-4");
    }

    #[test]
    fn checked_sub_rejects_underflow_and_allows_valid() {
        let five = Amount::parse_pms("5").unwrap();
        let three = Amount::parse_pms("3").unwrap();
        println!("5.checked_sub(3) = {:?}", five.checked_sub(three));
        println!("3.checked_sub(5) = {:?}", three.checked_sub(five));
        assert_eq!(five.checked_sub(three), Some(Amount::parse_pms("2").unwrap()));
        assert_eq!(three.checked_sub(five), None, "5 > 3 → insufficient → None");
        // Bord exact : balance == débit → Some(0), pas None.
        assert_eq!(five.checked_sub(five), Some(Amount::zero()));
    }

    #[test]
    fn division_uses_bankers_rounding_to_8dp() {
        // round_dp par défaut = half-to-even (banker's). Le déterminisme des fees
        // cross-node en dépend : 0.000000005 → 0 (pair), 0.000000015 → 0.00000002.
        let h1 = Amount::from_decimal(Decimal::from_str_exact("0.000000005").unwrap());
        let h2 = Amount::from_decimal(Decimal::from_str_exact("0.000000015").unwrap());
        println!("round 0.000000005={h1} ; 0.000000015={h2}");
        assert_eq!(h1.to_string(), "0");
        assert_eq!(h2.to_string(), "0.00000002");
        let one = Amount::parse_pms("1").unwrap();
        let three = Amount::parse_pms("3").unwrap();
        println!("1 / 3 = {}", one / three);
        assert_eq!((one / three).to_string(), "0.33333333");
    }

    #[test]
    #[should_panic]
    fn division_by_zero_panics() {
        // CARACTÉRISATION : div par Amount nul panique (rust_decimal). Les chemins
        // financiers ne doivent jamais diviser par un Amount non validé non-nul.
        let one = Amount::parse_pms("1").unwrap();
        let _ = one / Amount::zero();
    }

    #[test]
    #[should_panic]
    fn multiplication_overflow_panics() {
        // CARACTÉRISATION : un produit dépassant Decimal::MAX panique (pas de wrap
        // silencieux). Préférable à un wrap, mais à connaître pour les gros montants.
        let huge = Amount(Decimal::MAX);
        let _ = huge * Amount::parse_pms("2").unwrap();
    }

    #[test]
    fn parse_format_roundtrip_preserves_value_not_string() {
        // Display normalise (strip des zéros trailing) : le round-trip conserve la
        // VALEUR, pas la chaîne exacte. "10.50000000" → "10.5" → re-parse == valeur.
        let a = Amount::parse_pms("10.50000000").unwrap();
        println!("'10.50000000' displays as '{a}'");
        assert_eq!(a.to_string(), "10.5");
        let b = Amount::parse_pms(&a.to_string()).unwrap();
        assert_eq!(a, b, "value round-trips through parse→Display→parse");
    }
}
