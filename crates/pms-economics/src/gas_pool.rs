use pms_types_economics::GasPool;
use rust_decimal::Decimal;

/// Check if the gas pool has enough balance to consume `gas_per_tx`,
/// while staying above `min_balance` after consumption.
pub fn can_consume(pool: &GasPool, gas_per_tx: Decimal, min_balance: Decimal) -> bool {
    pool.balance >= gas_per_tx && (pool.balance - gas_per_tx) >= min_balance
}

/// Consume gas from the pool. Returns `Err` if balance is insufficient
/// (below `min_balance` after deduction).
///
/// On success, returns the new balance.
pub fn consume(
    pool: &mut GasPool,
    gas_per_tx: Decimal,
    min_balance: Decimal,
) -> Result<Decimal, GasPoolError> {
    if gas_per_tx.is_zero() {
        return Ok(pool.balance);
    }
    if !can_consume(pool, gas_per_tx, min_balance) {
        return Err(GasPoolError::InsufficientGas {
            balance: pool.balance,
            required: gas_per_tx,
            min_balance,
        });
    }
    pool.balance -= gas_per_tx;
    pool.total_consumed += gas_per_tx;
    Ok(pool.balance)
}

/// Deposit PMS into the gas pool. Returns the new balance.
pub fn deposit(pool: &mut GasPool, amount: Decimal) -> Result<Decimal, GasPoolError> {
    if amount <= Decimal::ZERO {
        return Err(GasPoolError::InvalidAmount(amount));
    }
    pool.balance += amount;
    pool.total_deposited += amount;
    Ok(pool.balance)
}

/// Withdraw PMS from the gas pool. Returns the new balance.
/// Cannot withdraw below zero.
pub fn withdraw(pool: &mut GasPool, amount: Decimal) -> Result<Decimal, GasPoolError> {
    if amount <= Decimal::ZERO {
        return Err(GasPoolError::InvalidAmount(amount));
    }
    if pool.balance < amount {
        return Err(GasPoolError::InsufficientBalance {
            balance: pool.balance,
            requested: amount,
        });
    }
    pool.balance -= amount;
    Ok(pool.balance)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GasPoolError {
    InsufficientGas {
        balance: Decimal,
        required: Decimal,
        min_balance: Decimal,
    },
    InsufficientBalance {
        balance: Decimal,
        requested: Decimal,
    },
    InvalidAmount(Decimal),
}

impl std::fmt::Display for GasPoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientGas {
                balance,
                required,
                min_balance,
            } => write!(
                f,
                "Insufficient gas: balance={balance}, required={required}, min_balance={min_balance}"
            ),
            Self::InsufficientBalance {
                balance,
                requested,
            } => write!(
                f,
                "Insufficient balance for withdrawal: balance={balance}, requested={requested}"
            ),
            Self::InvalidAmount(a) => write!(f, "Invalid amount: {a}"),
        }
    }
}

impl std::error::Error for GasPoolError {}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn make_pool(balance: Decimal) -> GasPool {
        let mut p = GasPool::new("test".into(), 0);
        p.balance = balance;
        p
    }

    #[test]
    fn consume_happy_path() {
        let mut pool = make_pool(dec!(100));
        let new_bal = consume(&mut pool, dec!(0.001), dec!(0)).unwrap();
        println!("new_balance={new_bal} total_consumed={}", pool.total_consumed);
        assert_eq!(new_bal, dec!(99.999));
        assert_eq!(pool.total_consumed, dec!(0.001));
    }

    #[test]
    fn consume_respects_min_balance() {
        let mut pool = make_pool(dec!(10));
        // gas_per_tx=1, min_balance=10 → after deduction balance=9 < 10 → rejected
        let err = consume(&mut pool, dec!(1), dec!(10)).unwrap_err();
        println!("Expected rejection: {err}");
        assert!(matches!(err, GasPoolError::InsufficientGas { .. }));
        // Balance unchanged
        assert_eq!(pool.balance, dec!(10));
    }

    #[test]
    fn consume_exact_threshold() {
        let mut pool = make_pool(dec!(10.001));
        // gas_per_tx=0.001, min_balance=10 → after deduction balance=10.0 >= 10 → OK
        let new_bal = consume(&mut pool, dec!(0.001), dec!(10)).unwrap();
        println!("new_balance={new_bal}");
        assert_eq!(new_bal, dec!(10));
    }

    #[test]
    fn consume_zero_gas_is_noop() {
        let mut pool = make_pool(dec!(5));
        let new_bal = consume(&mut pool, dec!(0), dec!(0)).unwrap();
        assert_eq!(new_bal, dec!(5));
        assert_eq!(pool.total_consumed, Decimal::ZERO);
    }

    #[test]
    fn deposit_happy_path() {
        let mut pool = make_pool(dec!(10));
        let new_bal = deposit(&mut pool, dec!(50)).unwrap();
        println!("new_balance={new_bal} total_deposited={}", pool.total_deposited);
        assert_eq!(new_bal, dec!(60));
        assert_eq!(pool.total_deposited, dec!(50));
    }

    #[test]
    fn deposit_zero_rejected() {
        let mut pool = make_pool(dec!(10));
        let err = deposit(&mut pool, dec!(0)).unwrap_err();
        println!("Expected rejection: {err}");
        assert!(matches!(err, GasPoolError::InvalidAmount(_)));
    }

    #[test]
    fn deposit_negative_rejected() {
        let mut pool = make_pool(dec!(10));
        let err = deposit(&mut pool, dec!(-5)).unwrap_err();
        println!("Expected rejection: {err}");
        assert!(matches!(err, GasPoolError::InvalidAmount(_)));
    }

    #[test]
    fn withdraw_happy_path() {
        let mut pool = make_pool(dec!(100));
        let new_bal = withdraw(&mut pool, dec!(30)).unwrap();
        println!("new_balance={new_bal}");
        assert_eq!(new_bal, dec!(70));
    }

    #[test]
    fn withdraw_more_than_balance() {
        let mut pool = make_pool(dec!(10));
        let err = withdraw(&mut pool, dec!(20)).unwrap_err();
        println!("Expected rejection: {err}");
        assert!(matches!(err, GasPoolError::InsufficientBalance { .. }));
        assert_eq!(pool.balance, dec!(10));
    }

    #[test]
    fn can_consume_checks() {
        let pool = make_pool(dec!(10));
        assert!(can_consume(&pool, dec!(0.001), dec!(0)));
        assert!(can_consume(&pool, dec!(10), dec!(0)));
        assert!(!can_consume(&pool, dec!(10.001), dec!(0)));
        assert!(!can_consume(&pool, dec!(1), dec!(10)));
        assert!(can_consume(&pool, dec!(1), dec!(9)));
    }
}
