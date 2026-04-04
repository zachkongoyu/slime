use serde::Deserialize;
use serde_json::Value;

use super::blackboard::GapType;

/// The deserialized form of the LLM's decomposition output.
#[derive(Debug, Deserialize)]
pub(crate) struct Decomposition {
    pub(crate) intent: Option<String>,
    pub(crate) gaps: Option<Vec<GapSpec>>,
}

/// One gap as described by the LLM — consumed once to build a `Gap` and insert it into the Blackboard.
#[derive(Debug, Deserialize)]
pub(crate) struct GapSpec {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) gap_type: GapType,
    pub(crate) dependencies: Vec<String>,
    pub(crate) constraints: Option<Value>,
    pub(crate) expected_output: Option<String>,
}
