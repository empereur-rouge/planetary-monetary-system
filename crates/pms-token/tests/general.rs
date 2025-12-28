use pms_token::{Amount, AmountError};

#[test]
fn parse_ok() {
    let a = Amount::parse("10.00000000", 8).unwrap();
    assert_eq!(a.to_string(), "10");
}

#[test]
fn too_many_decimals() {
    let e = Amount::parse("1.000000001", 8).unwrap_err();
    matches!(e, AmountError::TooManyDecimals(8));
}