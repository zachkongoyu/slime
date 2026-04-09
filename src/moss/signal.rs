use tokio::sync::broadcast;

/// All events emitted on the signal bus.
#[derive(Debug, Clone)]
pub enum Event {
    /// Full blackboard state snapshot — emitted after every mutation.
    Snapshot(Box<str>),
    /// A gap was held by the ArtifactGuard and needs human approval before proceeding.
    ApprovalRequested {
        gap_id: uuid::Uuid,
        gap_name: Box<str>,
        reason: Box<str>,
    },
    /// The Solver asked the human a question (via an `~~~ask` block).
    QuestionAsked {
        gap_id: uuid::Uuid,
        gap_name: Box<str>,
        question: Box<str>,
    },
}

pub type Payload = Event;

pub(crate) fn channel(capacity: usize) -> (broadcast::Sender<Payload>, broadcast::Receiver<Payload>) {
    broadcast::channel(capacity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscriber_receives_snapshot() {
        let (tx, mut rx) = channel(16);
        let _ = tx.send(Event::Snapshot("hello".into()));
        assert!(matches!(rx.recv().await.unwrap(), Event::Snapshot(_)));
    }

    #[tokio::test]
    async fn no_subscriber_does_not_panic() {
        let (tx, _) = channel(16);
        let _ = tx.send(Event::Snapshot("ignored".into()));
    }

    #[tokio::test]
    async fn independent_subscribers_each_get_the_event() {
        let (tx, mut rx1) = channel(16);
        let mut rx2 = tx.subscribe();
        let _ = tx.send(Event::Snapshot("ping".into()));
        assert!(matches!(rx1.recv().await.unwrap(), Event::Snapshot(_)));
        assert!(matches!(rx2.recv().await.unwrap(), Event::Snapshot(_)));
    }
}
