#[derive(Debug, Clone)]
pub struct Token {
    pub symbol: &'static str,
    pub decimals: u32,
}

impl Token {
    pub const fn new(symbol: &'static str, decimals: u32) -> Self {
        Self { symbol, decimals }
    }
}

/// Ton token natif (modifiable si besoin)
pub const PLANETARY_MONETARY_SYSTEM: Token = Token::new("PMS", 8);
