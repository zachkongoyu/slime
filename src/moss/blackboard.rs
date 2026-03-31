use dashmap::DashMap;
use serde_json::Value;
use uuid::Uuid;

#[derive(serde::Serialize)]
pub(in crate::moss) enum GapState {
    Blocked,
    Ready,
    Assigned,
    Closed,
}

#[derive(serde::Serialize)]
pub(in crate::moss) enum Pulse {
    Network,
    Machine,
    Other(String),
}

#[derive(serde::Serialize)]
pub(in crate::moss) struct Gap {
    gap_id: uuid::Uuid,
    state: GapState,
    description: Box<str>,
    pulse: Pulse,
}

impl Gap {
    pub(in crate::moss) fn new(state: GapState, description: Box<str>, pulse: Pulse) -> Self {
        Self {
            gap_id: Uuid::new_v4(),
            state,
            description,
            pulse,
        }
    }

    pub(in crate::moss) fn set_state(&mut self, new_state: GapState) {
        self.state = new_state;
    }
}

#[derive(serde::Serialize)]
pub(in crate::moss) struct Evidence {
    gap_id: uuid::Uuid,
    content: Value,
    done: bool,
}

impl Evidence {
    pub(in crate::moss) fn new(gap_id: uuid::Uuid, content: Value) -> Self {
        Self {
            gap_id,
            content,
            done: false,
        }
    }
}

#[derive(serde::Serialize)]
pub struct Blackboard {
    intent: Option<Box<str>>,
    /// evidences keyed by `gap_id` (single evidence per gap)
    evidences: DashMap<uuid::Uuid, Evidence>,
    gaps: DashMap<uuid::Uuid, Gap>,
    gates: DashMap<Box<str>, Value>,
}

impl Blackboard {
    pub fn new() -> Self {
        Self {
            intent: None,
            evidences: DashMap::new(),
            gaps: DashMap::new(),
            gates: DashMap::new(),
        }
    }

    pub(in crate::moss) fn insert_gap(&self, gap: Gap) {
        self.gaps.insert(gap.gap_id, gap);
    }

    pub(in crate::moss) fn set_gap_state(&self, gap_id: &uuid::Uuid, state: GapState) {
        let mut g = self.gaps.get_mut(gap_id).expect("gap not found");
        g.state = state;
    }

    pub(in crate::moss) fn insert_evidence(&self, ev: Evidence) {
        self.evidences.insert(ev.gap_id, ev);
    }

}

