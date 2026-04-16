use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

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
    pub(super) name: Box<str>,
    pub(super) state: GapState,
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

// ── BlackboardSnapshot ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BlackboardSnapshot {
    intent:    Option<Box<str>>,
    gaps:      HashMap<Uuid, Gap>,
    evidences: HashMap<Uuid, Vec<Evidence>>,
}

impl BlackboardSnapshot {
    pub(crate) fn new(
        intent:    Option<Box<str>>,
        gaps:      HashMap<Uuid, Gap>,
        evidences: HashMap<Uuid, Vec<Evidence>>,
    ) -> Self {
        Self { intent, gaps, evidences }
    }

    pub(crate) fn intent(&self) -> Option<&str> { self.intent.as_deref() }
    pub(crate) fn gaps(&self) -> impl Iterator<Item = &Gap> { self.gaps.values() }
    pub(crate) fn gap_state(&self, name: &str) -> Option<&GapState> {
        self.gaps.values().find(|g| g.name() == name).map(|g| g.state())
    }
    pub(crate) fn evidence_count(&self) -> usize {
        self.evidences.values().map(|v| v.len()).sum()
    }

    pub(crate) fn into_parts(self) -> (Option<Box<str>>, HashMap<Uuid, Gap>, HashMap<Uuid, Vec<Evidence>>) {
        (self.intent, self.gaps, self.evidences)
    }
}
