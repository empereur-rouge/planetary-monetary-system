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
        matches!(e, AmountError::TooManyDecimals(8));
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
}
