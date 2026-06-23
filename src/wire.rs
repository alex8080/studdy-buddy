//! Wire types: the JSON request/response bodies of the HTTP API.
//!
//! These are shared between the server (`api.rs` handlers) and the in-crate
//! HTTP client (`client.rs`), so the contract has a single definition and the
//! two can't drift. The domain types they carry (`Card`, `CardContent`,
//! `Rating`) live in [`crate::model`]; this module is just the envelopes.
//!
//! Every type derives both `Serialize` and `Deserialize`: the server uses one
//! direction per type, the client the other.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{Card, CardContent, CardId, Rating};

/// JSON key carrying the message in a non-2xx error body (`{ "error": "..." }`).
/// Written by the server, read by the client.
pub const ERROR_KEY: &str = "error";

/// Static endpoint paths, shared by the server router and the client so they
/// can't drift. Parameterized routes (those with a `{id}` segment) stay inline:
/// the server uses an axum template, the client a `format!`, so there's no
/// single literal to share.
pub mod path {
    pub const INGEST: &str = "/ingest";
    pub const CARDS_PENDING: &str = "/cards/pending";
    pub const CARDS_DUE: &str = "/cards/due";
    pub const REVIEWS: &str = "/reviews";
}

/// `POST /ingest` request — one pushed note: its vault-relative path and raw
/// markdown. The feeder (watcher or CLI) sends these per file; the server
/// parses, chunks, and proposes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestRequest {
    pub source_file: String,
    pub content: String,
}

/// `POST /ingest` response — per-note counts (best-effort over chunks).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestResponse {
    pub chunks: usize,
    pub proposed_cards: usize,
    pub failed_chunks: usize,
    pub skipped_chunks: usize,
}

/// Response envelope for `GET /cards/pending` and `GET /cards/due`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardsResponse {
    pub cards: Vec<Card>,
}

/// `PATCH /cards/{id}` request — content-only edit of a pending card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateContentRequest {
    pub content: CardContent,
}

/// `POST /reviews` request — a user's review of one card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub card_id: CardId,
    pub rating: Rating,
}

/// `POST /reviews` response — the card's next scheduling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewResponse {
    pub next_due: DateTime<Utc>,
    pub interval_days: u32,
}
