use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type CardId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CardContent {
    Qa { front: String, back: String },
    Cloze { text: String, spans: Vec<ClozeSpan> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClozeSpan {
    pub start: usize,
    pub end: usize,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CardStatus {
    Pending,
    Accepted,
    Orphaned,
    Stale,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Card {
    pub id: CardId,
    pub content: CardContent,
    pub source_file: String,
    pub source_heading: Option<String>,
    pub tags: Vec<String>,
    pub status: CardStatus,
    pub created_at: DateTime<Utc>,
}

/// User-facing review outcome — the four-button rating from Anki / SM-2 / FSRS.
///
/// Note: `Again` is semantically on a different axis than `Hard`/`Good`/`Easy`
/// — it answers "did you recall it?" rather than "how well?" — but we keep
/// this naming and variant set because it matches the ecosystem (`fsrs-rs`
/// and Anki UI use these exact four labels), which keeps the SM-2 → FSRS
/// swap trivial.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Rating {
    Again,
    Hard,
    Good,
    Easy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub card_id: CardId,
    pub reviewed_at: DateTime<Utc>,
    pub rating: Rating,
    pub next_due: DateTime<Utc>,
}
