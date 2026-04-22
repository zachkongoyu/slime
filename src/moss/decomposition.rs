use serde::{Deserialize, Serialize, Deserializer};
use serde_json::Value;

/// Custom deserializer that treats null as false for is_follow_up.
fn deserialize_is_follow_up<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<bool>::deserialize(deserializer)?;
    Ok(opt.unwrap_or(false))
}

/// The deserialized form of the LLM''s decomposition output.
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct Decomposition {
    pub(crate) intent: Option<String>,
    pub(crate) gaps: Option<Vec<GapSpec>>,
    #[serde(default, deserialize_with = "deserialize_is_follow_up")]
    pub(crate) is_follow_up: bool,
}

/// One gap as described by the LLM — consumed once to build a `Gap` and insert it into the Blackboard.
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct GapSpec {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) dependencies: Vec<String>,
    pub(crate) constraints: Option<Value>,
    pub(crate) expected_output: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_with_null_is_follow_up() {
        let json = r#"{"intent":"test","is_follow_up":null,"gaps":null}"#;
        let result: Result<Decomposition, _> = serde_json::from_str(json);
        assert!(result.is_ok(), "should deserialize null is_follow_up as false");
        assert_eq!(result.unwrap().is_follow_up, false);
    }

    #[test]
    fn deserialize_with_missing_is_follow_up() {
        let json = r#"{"intent":"test","gaps":null}"#;
        let result: Result<Decomposition, _> = serde_json::from_str(json);
        assert!(result.is_ok(), "should deserialize missing is_follow_up as false");
        assert_eq!(result.unwrap().is_follow_up, false);
    }

    #[test]
    fn deserialize_with_explicit_true() {
        let json = r#"{"intent":"test","is_follow_up":true,"gaps":null}"#;
        let result: Result<Decomposition, _> = serde_json::from_str(json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().is_follow_up, true);
    }
}
