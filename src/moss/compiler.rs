use std::sync::Arc;

use minijinja::{Environment, context};
use serde::Deserialize;

use crate::error::MossError;
use crate::providers::{Message, Role, Provider};

use super::blackboard::{Gap, GapType};

// ── Artifact ──────────────────────────────────────────────────────────────────

/// The compiled output for a single Gap, ready for the Executor to run.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum Artifact {
    Script {
        language: Box<str>,
        code: Box<str>,
        timeout_secs: u64,
    },
    Agent {
        role: Box<str>,
        goal: Box<str>,
        tools: Vec<Box<str>>,
        instructions: Box<str>,
    },
}

// ── Compiler ──────────────────────────────────────────────────────────────────

pub(crate) struct Compiler {
    provider: Arc<dyn Provider>,
}

impl Compiler {
    pub(crate) fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }

    /// Ask the LLM to produce an Artifact for the given Gap.
    /// `prior_attempts` contains error messages from previous failed runs, if any.
    pub(crate) async fn compile(
        &self,
        gap: &Gap,
        prior_attempts: &[Box<str>],
    ) -> Result<Artifact, MossError> {
        let template_src = include_str!("prompts/compiler.md");

        let gap_type_str = match gap.gap_type() {
            GapType::Proactive => "PROACTIVE",
            GapType::Reactive  => "REACTIVE",
        };

        let prior_str = if prior_attempts.is_empty() {
            "None".to_string()
        } else {
            prior_attempts
                .iter()
                .enumerate()
                .map(|(i, e)| format!("{}. {}", i + 1, e))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut env = Environment::new();
        env.add_template("compiler", template_src)
            .map_err(|e| MossError::Blackboard(format!("template error: {e}")))?;

        let tmpl = env
            .get_template("compiler")
            .map_err(|e| MossError::Blackboard(format!("template load error: {e}")))?;

        let rendered = tmpl
            .render(context! {
                gap_name        => gap.name(),
                gap_description => gap.description(),
                gap_type        => gap_type_str,
                prior_attempts  => prior_str,
            })
            .map_err(|e| MossError::Blackboard(format!("template render error: {e}")))?;

        let messages = vec![Message { role: Role::User, content: rendered.into_boxed_str() }];

        let raw = self.provider.complete_chat(messages).await?;

        let json_str = raw
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        let artifact: Artifact = serde_json::from_str(json_str)?;

        Ok(artifact)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::error::ProviderError;
    use crate::providers::{Message, Provider};
    use crate::moss::blackboard::{Gap, GapType};

    use super::{Artifact, Compiler};

    struct MockCompilerProvider {
        response: String,
    }

    #[async_trait]
    impl Provider for MockCompilerProvider {
        async fn complete_chat(&self, _messages: Vec<Message>) -> Result<String, ProviderError> {
            Ok(self.response.clone())
        }
    }

    fn make_gap(name: &str, gap_type: GapType) -> Gap {
        Gap::new(name, "Test description", gap_type, vec![], None, None)
    }

    #[tokio::test]
    async fn compile_proactive_returns_script() {
        let provider = Arc::new(MockCompilerProvider {
            response: r#"{
                "type": "SCRIPT",
                "language": "python",
                "code": "print('{\"result\": 42}')",
                "timeout_secs": 10
            }"#.to_string(),
        });

        let artifact = Compiler::new(provider)
            .compile(&make_gap("add_numbers", GapType::Proactive), &[])
            .await
            .unwrap();

        match artifact {
            Artifact::Script { language, timeout_secs, .. } => {
                assert_eq!(&*language, "python");
                assert_eq!(timeout_secs, 10);
            }
            _ => panic!("expected Script"),
        }
    }

    #[tokio::test]
    async fn compile_reactive_returns_agent() {
        let provider = Arc::new(MockCompilerProvider {
            response: r#"{
                "type": "AGENT",
                "role": "Web Scout",
                "goal": "Find BTC price",
                "tools": ["web_search"],
                "instructions": "Search and return price."
            }"#.to_string(),
        });

        let artifact = Compiler::new(provider)
            .compile(&make_gap("fetch_price", GapType::Reactive), &[])
            .await
            .unwrap();

        match artifact {
            Artifact::Agent { role, tools, .. } => {
                assert_eq!(&*role, "Web Scout");
                assert!(tools.iter().any(|t| &**t == "web_search"));
            }
            _ => panic!("expected Agent"),
        }
    }

    #[tokio::test]
    async fn compile_strips_markdown_fences() {
        let provider = Arc::new(MockCompilerProvider {
            response: "```json\n{\"type\":\"SCRIPT\",\"language\":\"shell\",\"code\":\"echo hi\",\"timeout_secs\":5}\n```".to_string(),
        });

        let artifact = Compiler::new(provider)
            .compile(&make_gap("say_hi", GapType::Proactive), &[])
            .await
            .unwrap();

        match artifact {
            Artifact::Script { language, .. } => assert_eq!(&*language, "shell"),
            _ => panic!("expected Script"),
        }
    }

    #[tokio::test]
    async fn compile_handles_prior_attempts() {
        let provider = Arc::new(MockCompilerProvider {
            response: r#"{"type":"SCRIPT","language":"python","code":"pass","timeout_secs":10}"#
                .to_string(),
        });

        let attempts: Vec<Box<str>> = vec![
            "Timeout after 30s".into(),
            "SyntaxError on line 3".into(),
        ];

        Compiler::new(provider)
            .compile(&make_gap("retry_gap", GapType::Proactive), &attempts)
            .await
            .unwrap();
    }
}
