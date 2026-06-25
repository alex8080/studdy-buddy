pub mod ollama;
pub mod prompt;
pub mod retry;

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::model::CardContent;

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

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn propose_cards(&self, chunk: &ChunkContext) -> Result<Vec<ProposedCard>, LlmError>;
}
