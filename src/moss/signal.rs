use std::collections::HashMap;

use tokio::sync::oneshot;
use uuid::Uuid;

use super::types::{BlackboardSnapshot, Evidence, Gap};

#[derive(Debug)]
pub enum Event {
    BlackboardSnapshot {
        intent:    Option<Box<str>>,
        gaps:      HashMap<Uuid, Gap>,
        evidences: HashMap<Uuid, Vec<Evidence>>,
    },
    SolverProgress {
        gap_id:         Uuid,
        gap_name:       Box<str>,
        iteration:      u32,
        max_iterations: u32,
        step:           Box<str>,
        last_result:    Option<Box<str>>,
    },
    Approval {
        gap_id:   Uuid,
        gap_name: Box<str>,
        reason:   Box<str>,
        tx:       oneshot::Sender<bool>,
    },
    Question {
        gap_id:   Uuid,
        gap_name: Box<str>,
        question: Box<str>,
        tx:       oneshot::Sender<String>,
    },
}

impl From<BlackboardSnapshot> for Event {
    fn from(snap: BlackboardSnapshot) -> Self {
        let (intent, gaps, evidences) = snap.into_parts();
        Self::BlackboardSnapshot { intent, gaps, evidences }
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use super::*;
    use crate::moss::blackboard::Blackboard;

    #[tokio::test]
    async fn receiver_gets_snapshot() {
        let (btx, _brx) = mpsc::channel::<Event>(1);
        let bb = Blackboard::new(btx);
        let snap = bb.snapshot();
        let (tx, mut rx) = mpsc::channel::<Event>(16);
        tx.try_send(snap.into()).ok();
        assert!(matches!(rx.recv().await.unwrap(), Event::BlackboardSnapshot { .. }));
    }

    #[tokio::test]
    async fn send_with_no_receiver_does_not_panic() {
        let (btx, _brx) = mpsc::channel::<Event>(1);
        let bb = Blackboard::new(btx);
        let snap = bb.snapshot();
        let (tx, rx) = mpsc::channel::<Event>(16);
        drop(rx);
        tx.try_send(snap.into()).ok();
    }
}
