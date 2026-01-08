//! Event Bus implémentation via `tokio::broadcast`.
//!
//! Le bus utilise un canal broadcast qui permet :
//! - Multiple subscribers (plusieurs handlers peuvent écouter)
//! - Async/await natif
//! - Backpressure automatique (les subscribers lents perdent des events)
//!
//! ## Voir aussi
//! - Tokio broadcast documentation : https://docs.rs/tokio/latest/tokio/sync/broadcast/

use crate::PmsEvent;
use tokio::sync::broadcast;
use tracing::debug;

/// Bus d'événements central pour PMS.
///
/// Ce bus est thread-safe et peut être cloné (Arc interne via Sender).
/// Chaque clone partage le même canal sous-jacent.
///
/// # Exemple
///
/// ```rust,ignore
/// let bus = EventBus::new(1024);
/// let bus_clone = bus.clone(); // Partage le même canal
///
/// // Émettre depuis n'importe quel clone
/// bus_clone.emit(PmsEvent::BlockAdded { block_id: "abc".into() });
/// ```
#[derive(Clone)]
pub struct EventBus {
    /// Sender broadcast - permet d'émettre et de créer des receivers
    sender: broadcast::Sender<PmsEvent>,
}

impl EventBus {
    /// Crée un nouveau bus d'événements.
    ///
    /// # Arguments
    ///
    /// * `capacity` - Taille du buffer. Les subscribers qui prennent du retard
    ///   perdront les événements les plus anciens (lagged).
    ///   Recommandé : 1024 pour un usage normal, 4096 pour haut débit.
    ///
    /// # Voir aussi
    /// Chapitre 16 du Rust Book : Fearless Concurrency
    /// https://doc.rust-lang.org/book/ch16-00-concurrency.html
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Émet un événement sur le bus.
    ///
    /// L'émission est non-bloquante. Si aucun subscriber n'écoute,
    /// l'événement est simplement ignoré (pas de panic).
    ///
    /// # Arguments
    ///
    /// * `event` - L'événement à émettre
    ///
    /// # Performance
    /// Cette opération est O(n) où n = nombre de subscribers.
    /// Chaque subscriber reçoit une copie (Clone) de l'événement.
    pub fn emit(&self, event: PmsEvent) {
        // Log l'événement pour le debugging
        debug!(event_type = %event.event_type(), block_id = %event.block_id(), "📢 Event emitted");

        // send() retourne Err si aucun receiver n'est connecté
        // C'est normal au démarrage ou si tous les handlers sont arrêtés
        // NOTE: is_err() est plus idiomatique que if let Err(_)
        if self.sender.send(event).is_err() {
            // Pas d'erreur, juste un warning en debug
            // En prod, c'est souvent normal (pas de subscriber actif)
            debug!("No active subscribers for event");
        }
    }

    /// Crée un nouveau subscriber pour écouter les événements.
    ///
    /// Le receiver retourné implémente `Stream` et peut être utilisé
    /// avec `.recv().await` dans une boucle async.
    ///
    /// # Attention
    ///
    /// Si le subscriber ne consomme pas assez vite, il recevra une
    /// erreur `Lagged` et perdra les événements manqués.
    ///
    /// # Exemple typique
    ///
    /// ```rust,ignore
    /// let mut rx = bus.subscribe();
    /// loop {
    ///     match rx.recv().await {
    ///         Ok(event) => handle_event(event),
    ///         Err(broadcast::error::RecvError::Lagged(n)) => {
    ///             warn!("Dropped {} events", n);
    ///         }
    ///         Err(broadcast::error::RecvError::Closed) => break,
    ///     }
    /// }
    /// ```
    pub fn subscribe(&self) -> broadcast::Receiver<PmsEvent> {
        self.sender.subscribe()
    }

    /// Retourne le nombre actuel de subscribers.
    ///
    /// Utile pour le monitoring et les health checks.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for EventBus {
    /// Crée un bus avec une capacité par défaut de 1024.
    fn default() -> Self {
        Self::new(1024)
    }
}
