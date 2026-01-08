//! # pms-event
//!
//! Système d'événements asynchrone pour PMS.
//!
//! Ce crate fournit un bus d'événements basé sur `tokio::broadcast`
//! permettant à différents modules de s'abonner et réagir aux événements
//! on-chain (NFT brûlé, smart contract exécuté, etc.).
//!
//! ## Exemple d'utilisation
//!
//! ```rust,ignore
//! use pms_event::{EventBus, PmsEvent};
//!
//! // Créer le bus (capacité de 1024 événements en buffer)
//! let bus = EventBus::new(1024);
//!
//! // S'abonner aux événements
//! let mut rx = bus.subscribe();
//!
//! // Dans un autre thread/task, émettre un événement
//! bus.emit(PmsEvent::NftBurned {
//!     block_id: "abc123".into(),
//!     token_id: "nft-001".into(),
//!     burner_address: "8e1...".into(),
//! });
//!
//! // Le subscriber reçoit l'événement
//! // let event = rx.recv().await?;
//! ```

mod bus;
mod events;

pub use bus::EventBus;
pub use events::PmsEvent;
