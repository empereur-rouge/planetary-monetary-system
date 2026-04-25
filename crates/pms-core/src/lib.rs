// NOTE: Ces allow sont temporaires pour le pré-déploiement.
// Les collapsible_if peuvent être refactorisés après le déploiement.
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_match)]
#![allow(clippy::manual_is_ascii_check)]
#![allow(clippy::len_zero)]
#![allow(clippy::doc_overindented_list_items)]
#![allow(clippy::for_kv_map)]
#![allow(clippy::empty_line_after_doc_comments)]
#![allow(clippy::manual_is_multiple_of)]
#![allow(clippy::unwrap_or_default)]
#![allow(clippy::module_inception)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::single_match)]
pub mod background_persist;
mod block_builder;
pub mod concurrent_dag;
pub mod metrics;
mod core_adapter;
mod crypto;
pub mod dag;
pub mod finality;
pub mod forge;
pub mod net_adapter;
pub mod tips;
pub mod utxo;
pub mod validations;
mod weights;

pub use block_builder::BlockMineBuilder;
pub use concurrent_dag::ConcurrentDag;
pub use core_adapter::*;
pub use dag::*;
pub use finality::*;
pub use forge::*;
pub use validations::amount::*;
pub use validations::check::*;
pub use validations::fees::*;
pub use validations::policy::*;
