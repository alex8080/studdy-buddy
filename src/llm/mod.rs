pub mod ollama;
pub mod prompt;
pub mod retry;

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::model::{Card, CardContent, Verdict};

#[derive(Debug, Clone)]
pub enum LlmError {
    Transient {
        reason: String,
        retry_after: Option<Duration>,
    },
    BadInput {
        reason: String,
    },
    Config {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkContext {
    pub source_file: String,
    pub source_heading: Option<String>,
    pub tags: Vec<String>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedCard {
    pub content: CardContent,
    pub rationale: Option<String>,
}

/// The verdict returned by [`LlmProvider::evaluate_answer`].
#[derive(Debug, Clone)]
pub struct EvaluationResult {
    pub verdict: Verdict,
    pub explanation: String,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn propose_cards(&self, chunk: &ChunkContext) -> Result<Vec<ProposedCard>, LlmError>;

    /// Grade a student's free-text answer against a Q&A card.
    ///
    /// Returns `LlmError::BadInput` for cloze cards — the handler branches
    /// on card type and should never call this path for cloze.
    async fn evaluate_answer(
        &self,
        card: &Card,
        user_answer: &str,
    ) -> Result<EvaluationResult, LlmError>;
}
