use pms_token::{Amount, AmountError};

#[test]
fn parse_ok() {
    let a = Amount::parse("10.00000000", 8).unwrap();
    assert_eq!(a.to_string(), "10");
}

#[test]
fn too_many_decimals() {
    let e = Amount::parse("1.000000001", 8).unwrap_err();
    // CRITICAL: `matches!(...)` sans `assert!` (v0.9.2) renvoyait un bool jeté —
    // le test ne vérifiait que "une erreur a eu lieu", pas SA nature. Un retour
    // de AmountError::Parse au lieu de TooManyDecimals serait passé inaperçu.
    assert!(
        matches!(e, AmountError::TooManyDecimals(8)),
        "expected TooManyDecimals(8), got {e:?}"
    );
}

#[test]
fn parse_with_custom_precision_rejects_excess_decimals() {
    // Exerce le paramètre `max_decimals` non-8 du point d'entrée public `parse`,
    // jamais couvert par les tests inline `parse_pms` (toujours 8).
    let ok = Amount::parse("1.12", 2).expect("2 decimals within limit");
    assert_eq!(ok.to_string(), "1.12");

    let err = Amount::parse("1.123", 2).unwrap_err();
    assert!(
        matches!(err, AmountError::TooManyDecimals(2)),
        "expected TooManyDecimals(2), got {err:?}"
    );
}

#[test]
fn parse_rejects_negative() {
    let err = Amount::parse("-1.5", 8).unwrap_err();
    assert!(
        matches!(err, AmountError::Negative),
        "expected Negative, got {err:?}"
    );
}
