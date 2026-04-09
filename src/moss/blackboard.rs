use std::sync::Mutex;
use std::collections::HashMap;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::error::MossError;
use tokio::sync::{broadcast, oneshot};

use super::signal;

// ── Gap ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum GapState {
    Blocked,
    Ready,
    Assigned,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Gap {
    gap_id: Uuid,
    name: Box<str>,
    state: GapState,
    description: Box<str>,
    dependencies: Vec<Box<str>>,
    constraints: Option<Value>,
    expected_output: Option<Box<str>>,
}

impl Gap {
    pub(crate) fn new(
        name: impl Into<Box<str>>,
        description: impl Into<Box<str>>,
        dependencies: Vec<Box<str>>,
        constraints: Option<Value>,
        expected_output: Option<Box<str>>,
    ) -> Self {
        let deps = dependencies;
        let has_deps = !deps.is_empty();
        Self {
            gap_id: Uuid::new_v4(),
            name: name.into(),
            state: if has_deps { GapState::Blocked } else { GapState::Ready },
            description: description.into(),
            dependencies: deps,
            constraints,
            expected_output,
        }
    }

    pub(crate) fn gap_id(&self) -> Uuid { self.gap_id }
    pub(crate) fn name(&self) -> &str { &self.name }
    pub(crate) fn state(&self) -> &GapState { &self.state }
    pub(crate) fn description(&self) -> &str { &self.description }
    pub(crate) fn dependencies(&self) -> &[Box<str>] { &self.dependencies }
    pub(crate) fn constraints(&self) -> Option<&Value> { self.constraints.as_ref() }
    pub(crate) fn expected_output(&self) -> Option<&str> { self.expected_output.as_deref() }
    pub(crate) fn set_state(&mut self, state: GapState) { self.state = state; }
}

// ── Evidence ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum EvidenceStatus {
    Success,
    Failure { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Evidence {
    gap_id: Uuid,
    content: Value,
    status: EvidenceStatus,
}

impl Evidence {
    pub(crate) fn new(gap_id: Uuid, content: Value, status: EvidenceStatus) -> Self {
        Self { gap_id, content, status }
    }

    pub(crate) fn gap_id(&self) -> Uuid { self.gap_id }
    pub(crate) fn content(&self) -> &Value { &self.content }
    pub(crate) fn status(&self) -> &EvidenceStatus { &self.status }
}

// ── Blackboard ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct Blackboard {
    intent: Mutex<Option<Box<str>>>,
    gaps: DashMap<Uuid, Gap>,
    name_index: DashMap<Box<str>, Uuid>,
    evidences: DashMap<Uuid, Vec<Evidence>>,
    pending_approvals: DashMap<Uuid, oneshot::Sender<bool>>,
    pending_questions: DashMap<Uuid, oneshot::Sender<String>>,
    tx: broadcast::Sender<signal::Payload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BlackboardSnapshot {
    intent: Option<Box<str>>,
    gaps: HashMap<Uuid, Gap>,
    evidences: HashMap<Uuid, Vec<Evidence>>,
}

impl Blackboard {
    pub(crate) fn new(tx: broadcast::Sender<signal::Payload>) -> Self {
        Self {
            intent: Mutex::new(None),
            gaps: DashMap::new(),
            name_index: DashMap::new(),
            evidences: DashMap::new(),
            pending_approvals: DashMap::new(),
            pending_questions: DashMap::new(),
            tx,
        }
    }

    pub(crate) fn signal_tx(&self) -> &broadcast::Sender<signal::Payload> {
        &self.tx
    }

    pub(crate) fn set_intent(&self, intent: impl Into<Box<str>>) {
        *self.intent.lock().unwrap() = Some(intent.into());
        let _ = self.tx.send(self.snapshot_json());
    }

    pub(crate) fn get_intent(&self) -> Option<Box<str>> {
        self.intent.lock().unwrap().clone()
    }

    pub(crate) fn insert_gap(&self, gap: Gap) -> Result<(), MossError> {
        if self.name_index.contains_key(gap.name()) {
            return Err(MossError::Blackboard(format!(
                "gap '{}' already exists",
                gap.name()
            )));
        }
        let id = gap.gap_id();
        let name = gap.name.clone();
        self.gaps.insert(id, gap);
        self.name_index.insert(name, id);
        let _ = self.tx.send(self.snapshot_json());
        Ok(())
    }

    pub(crate) fn set_gap_state(&self, gap_id: &Uuid, new_state: GapState) -> Result<(), MossError> {
        self.gaps
            .get_mut(gap_id)
            .ok_or_else(|| MossError::Blackboard(format!("gap {gap_id} not found")))?
            .set_state(new_state);
        let _ = self.tx.send(self.snapshot_json());
        Ok(())
    }

    pub(crate) fn append_evidence(&self, ev: Evidence) {
        self.evidences.entry(ev.gap_id()).or_default().push(ev);
        let _ = self.tx.send(self.snapshot_json());
    }

    pub(crate) fn get_gap(&self, gap_id: &Uuid) -> Option<Gap> {
        self.gaps.get(gap_id).map(|g| g.clone())
    }

    pub(crate) fn get_gap_id_by_name(&self, name: &str) -> Option<Uuid> {
        self.name_index.get(name).map(|id| *id)
    }

    pub(crate) fn get_evidence(&self, gap_id: &Uuid) -> Vec<Evidence> {
        self.evidences.get(gap_id).map(|v| v.clone()).unwrap_or_default()
    }

    /// Promote every `Blocked` gap whose dependencies are all `Closed` → `Ready`.
    pub(crate) fn promote_unblocked(&self) {
        let to_promote: Vec<Uuid> = self
            .gaps
            .iter()
            .filter(|entry| entry.state == GapState::Blocked)
            .filter(|entry| {
                entry.dependencies().iter().all(|dep_name| {
                    self.name_index
                        .get(dep_name.as_ref())
                        .and_then(|id| self.gaps.get(&*id))
                        .map(|dep| dep.state == GapState::Closed)
                        .unwrap_or(false)
                })
            })
            .map(|entry| entry.gap_id())
            .collect();

        for id in to_promote {
            if let Some(mut g) = self.gaps.get_mut(&id) {
                g.state = GapState::Ready;
            }
        }
    }

    /// Take all `Ready` gaps, mark them `Assigned`, and return them.
    pub(crate) fn drain_ready(&self) -> Vec<Gap> {
        let ready_ids: Vec<Uuid> = self
            .gaps
            .iter()
            .filter(|entry| entry.state == GapState::Ready)
            .map(|entry| entry.gap_id())
            .collect();

        let mut taken = Vec::new();
        for id in ready_ids {
            if let Some(mut g) = self.gaps.get_mut(&id) {
                if g.state == GapState::Ready {
                    g.state = GapState::Assigned;
                    taken.push(g.clone());
                }
            }
        }
        taken
    }

    pub(crate) fn all_closed(&self) -> bool {
        self.gaps.iter().all(|g| g.state == GapState::Closed)
    }

    pub(crate) fn register_approval(&self, gap_id: Uuid, sender: oneshot::Sender<bool>) {
        self.pending_approvals.insert(gap_id, sender);
    }

    pub(crate) fn approve(&self, gap_id: Uuid, approved: bool) {
        if let Some((_, sender)) = self.pending_approvals.remove(&gap_id) {
            let _ = sender.send(approved);
        }
    }

    pub(crate) fn register_question(&self, gap_id: Uuid, sender: oneshot::Sender<String>) {
        self.pending_questions.insert(gap_id, sender);
    }

    pub(crate) fn answer_question(&self, gap_id: Uuid, answer: String) {
        if let Some((_, sender)) = self.pending_questions.remove(&gap_id) {
            let _ = sender.send(answer);
        }
    }

    pub(crate) fn all_evidence(&self) -> Vec<Evidence> {
        self.evidences
            .iter()
            .flat_map(|entry| entry.value().clone())
            .collect()
    }

    pub(crate) fn snapshot(&self) -> BlackboardSnapshot {
        BlackboardSnapshot {
            intent: self.intent.lock().unwrap().clone(),
            gaps: self.gaps.iter().map(|e| (*e.key(), e.value().clone())).collect(),
            evidences: self.evidences.iter().map(|e| (*e.key(), e.value().clone())).collect(),
        }
    }

    fn snapshot_json(&self) -> super::signal::Event {
        let json: Box<str> = serde_json::to_string(&self.snapshot()).unwrap_or_default().into();
        super::signal::Event::Snapshot(json)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::moss::signal;

    fn bb() -> Blackboard { Blackboard::new(signal::channel(1).0) }

    fn gap(name: &str, deps: Vec<&str>) -> Gap {
        Gap::new(
            name,
            format!("description for {name}"),
            deps.into_iter().map(|s| s.into()).collect(),
            None,
            None,
        )
    }

    #[test]
    fn linear_chain_promote_unblocked() {
        // A → B → C
        let bb = bb();
        bb.insert_gap(gap("A", vec![])).unwrap();
        bb.insert_gap(gap("B", vec!["A"])).unwrap();
        bb.insert_gap(gap("C", vec!["B"])).unwrap();

        let a_id = bb.get_gap_id_by_name("A").unwrap();
        let b_id = bb.get_gap_id_by_name("B").unwrap();
        let c_id = bb.get_gap_id_by_name("C").unwrap();

        assert_eq!(bb.get_gap(&a_id).unwrap().state(), &GapState::Ready);
        assert_eq!(bb.get_gap(&b_id).unwrap().state(), &GapState::Blocked);
        assert_eq!(bb.get_gap(&c_id).unwrap().state(), &GapState::Blocked);

        // Close A → B becomes Ready
        bb.set_gap_state(&a_id, GapState::Closed).unwrap();
        bb.promote_unblocked();
        assert_eq!(bb.get_gap(&b_id).unwrap().state(), &GapState::Ready);
        assert_eq!(bb.get_gap(&c_id).unwrap().state(), &GapState::Blocked);

        // Close B → C becomes Ready
        bb.set_gap_state(&b_id, GapState::Closed).unwrap();
        bb.promote_unblocked();
        assert_eq!(bb.get_gap(&c_id).unwrap().state(), &GapState::Ready);
    }

    #[test]
    fn parallel_fanout_drain_ready() {
        // X and Y are independent — both start Ready
        let bb = bb();
        bb.insert_gap(gap("X", vec![])).unwrap();
        bb.insert_gap(gap("Y", vec![])).unwrap();

        let drained = bb.drain_ready();
        assert_eq!(drained.len(), 2);

        let x_id = bb.get_gap_id_by_name("X").unwrap();
        let y_id = bb.get_gap_id_by_name("Y").unwrap();
        assert_eq!(bb.get_gap(&x_id).unwrap().state(), &GapState::Assigned);
        assert_eq!(bb.get_gap(&y_id).unwrap().state(), &GapState::Assigned);
    }

    #[test]
    fn all_closed_only_when_all_gaps_closed() {
        let bb = bb();
        bb.insert_gap(gap("P", vec![])).unwrap();
        bb.insert_gap(gap("Q", vec![])).unwrap();

        assert!(!bb.all_closed());

        let p_id = bb.get_gap_id_by_name("P").unwrap();
        let q_id = bb.get_gap_id_by_name("Q").unwrap();
        bb.set_gap_state(&p_id, GapState::Closed).unwrap();
        assert!(!bb.all_closed());
        bb.set_gap_state(&q_id, GapState::Closed).unwrap();
        assert!(bb.all_closed());
    }

    #[test]
    fn append_and_get_evidence() {
        let bb = bb();
        let gap_id = Uuid::new_v4();

        bb.append_evidence(Evidence::new(gap_id, json!({"result": 42}), EvidenceStatus::Success));
        bb.append_evidence(Evidence::new(gap_id, json!({"result": 99}), EvidenceStatus::Success));

        let evs = bb.get_evidence(&gap_id);
        assert_eq!(evs.len(), 2);
    }
}
