//! Persistence for `Card`, `Review`, and `SchedulerState`.
//!
//! Everything sits behind the [`Repository`] trait — api handlers and the
//! (not-yet-built) watcher depend only on the trait, never on a concrete
//! backend. [`InMemoryRepository`] is the test double; [`FileRepository`] is
//! the v1 file backend (cards one-file-per-note mirrored under the store root,
//! plus a single `state.json` and an append-only `reviews.jsonl`, one store per
//! ingested directory). A SQLite/Postgres impl follows if a multi-tenant hosted
//! mode arrives. See `docs/architecture.md` for the layout rationale.

mod file;
mod memory;

pub use file::FileRepository;
pub use memory::InMemoryRepository;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::model::{Card, CardContent, CardId, CardStatus, Review};
use crate::scheduler::SchedulerState;

/// Storage seam for cards and their review state.
///
/// Each method maps to a documented call site in `docs/api.md`; nothing here
/// is speculative. Async to match [`crate::llm::LlmProvider`] (the other I/O
/// trait) and to let an async `sqlx` backend drop in unchanged.
#[async_trait]
pub trait Repository: Send + Sync {
    /// Persist freshly proposed cards (status `Pending`). Used by `POST /ingest`.
    async fn save_pending(&self, cards: &[Card]) -> Result<()>;

    /// List cards awaiting curation. Used by `GET /cards/pending`.
    async fn list_pending(&self) -> Result<Vec<Card>>;

    /// Edit a card's content during curation. Used by `PATCH /cards/{id}`.
    ///
    /// Content-only and `Pending`-only: rejects the call if the card is not
    /// `Pending` (the curation fix-up invariant). The anchor, tags, and status
    /// are not editable here.
    async fn update_content(&self, card: CardId, content: CardContent) -> Result<()>;

    /// Move a card between lifecycle states (accept / reject / flag).
    async fn set_status(&self, card: CardId, status: CardStatus) -> Result<()>;

    /// `Accepted` cards whose `next_due <= now`. Used by `GET /cards/due`.
    /// Status-filtered so a card rejected after acceptance (lingering state)
    /// doesn't resurface as due.
    async fn list_due(&self, now: DateTime<Utc>) -> Result<Vec<Card>>;

    /// Load a card's current SRS state. Used by `POST /reviews`.
    async fn load_state(&self, card: CardId) -> Result<SchedulerState>;

    /// Persist a card's SRS state and current due date (after a review, or
    /// seeded on accept). `next_due` is stored alongside the state so it serves
    /// as the due-index for [`list_due`](Repository::list_due).
    async fn save_state(
        &self,
        card: CardId,
        state: SchedulerState,
        next_due: DateTime<Utc>,
    ) -> Result<()>;

    /// Append a review to the durable log. Used by `POST /reviews`.
    async fn save_review(&self, review: &Review) -> Result<()>;
}
