// pms-consensus
// Core consensus constants and rules.

/// The Public Key of the Coordinator (Master Node) for Mainnet.
/// Blocks signed by this key are treated as Milestones/Checkpoints.
pub const COORDINATOR_PUBLIC_KEY_MAINNET: &str =
    "036ed4d5ad1c927fe972ef9728ac1888d237af57a488b6cbe50228fac442b5ae6b";

/// The Public Key of the Coordinator for Testnet.
pub const COORDINATOR_PUBLIC_KEY_TESTNET: &str =
    "02115e0941c01a05f6d6dfc6aa9204e20d0d1af9d3231c25728034d9278bf7187f";
