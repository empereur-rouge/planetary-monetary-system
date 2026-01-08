// NOTE: Ces allow sont temporaires pour le pré-déploiement.
// Les collapsible_if peuvent être refactorisés après le déploiement.
#![allow(clippy::collapsible_if)]
#![allow(clippy::new_without_default)]
#![allow(clippy::unnecessary_cast)]
#![allow(unused_variables)]
mod admin;
pub mod api;
pub mod api_fn;

mod helper;
pub mod limits;
mod metrics;
mod rate;
pub mod server;
pub mod stats;
pub mod tls;

pub use config::*;
pub use helper::resolve_admin_token;
pub use server::Server;
