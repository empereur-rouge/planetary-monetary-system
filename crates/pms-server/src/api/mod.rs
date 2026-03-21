// pms-server/src/api — HTTP API module.
//
// Split into sub-modules for maintainability:
// - state:           AppState, FeePoolRefundSink, DAG size metric helpers
// - middleware:      Authentication and authorization middleware
// - routes:          Router construction (ledger-scoped, admin, full API)
// - serve:           TLS/HTTP server bootstrap (`serve_api`)
// - ledger_dispatch: Dynamic per-ledger request routing
// - tasks:           Background tasks (fee distribution, inflation mint)

mod state;
mod middleware;
mod routes;
mod serve;
mod ledger_dispatch;
mod tasks;

// Re-export public API — these are imported throughout the codebase
// as `crate::api::AppState`, `pms_server::api::AppState`, etc.
pub use state::AppState;
pub use state::FeePoolRefundSink;
pub use routes::build_api_router;
pub use serve::serve_api;
pub use tasks::{spawn_fee_distributor_task, spawn_inflation_mint_task, distribute_for_ledger};
