mod cache;
mod classify;
mod handler;
mod stream;

#[cfg(test)]
mod tests;

// Re-export all public items so that `crate::api_fn::activity::*` paths remain valid.
pub use cache::ActivityCache;
// classify functions are pub(crate) — accessed directly via crate::api_fn::activity::classify::*
pub use handler::{get_wallet_activity, ActivityItem, ActivityQuery, ActivityResp};
pub use stream::{stream_wallet_activity, StreamActivityQuery};
