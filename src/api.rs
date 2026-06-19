use std::path::Component;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;
use crate::ingest::{ChunkConfig, ingest_text};
use crate::llm::{LlmError, LlmProvider};
use crate::model::{Card, CardContent, CardId, CardStatus, Rating, Review};
use crate::scheduler::{Scheduler, SchedulerState};
use crate::store::Repository;

#[derive(Clone)]
pub struct AppState {
    pub llm: Arc<dyn LlmProvider>,
    pub store: Arc<dyn Repository>,
    pub scheduler: Arc<dyn Scheduler>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ingest", post(ingest))
        .route("/cards/pending", get(cards_pending))
        .route("/cards/due", get(cards_due))
        .route("/cards/{id}/accept", post(accept_card))
        .route("/cards/{id}/reject", post(reject_card))
        .route("/cards/{id}", patch(patch_card))
        .route("/reviews", post(post_review))
        .with_state(state)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

/// One pushed note: its vault-relative path and raw markdown. The feeder
/// (watcher) sends these per file; the server parses, chunks, and proposes.
#[derive(Deserialize)]
struct IngestRequest {
    source_file: String,
    content: String,
}

#[derive(Serialize)]
struct IngestResponse {
    chunks: usize,
    proposed_cards: usize,
    failed_chunks: usize,
    skipped_chunks: usize,
}

async fn ingest(
    State(state): State<AppState>,
    Json(req): Json<IngestRequest>,
) -> Result<Json<IngestResponse>, AppError> {
    validate_source_file(&req.source_file)?;
    let chunks = ingest_text(&req.content, &req.source_file, &ChunkConfig::default())?;

    let mut cards = Vec::new();
    let mut proposed_cards = 0usize;
    let mut failed_chunks = 0usize;
    let mut skipped_chunks = 0usize;
    for chunk in &chunks {
        match state.llm.propose_cards(chunk).await {
            Ok(proposed) => {
                proposed_cards += proposed.len();
                for p in proposed {
                    cards.push(Card {
                        id: Uuid::new_v4(),
                        content: p.content,
                        source_file: chunk.source_file.clone(),
                        source_heading: chunk.source_heading.clone(),
                        tags: chunk.tags.clone(),
                        status: CardStatus::Pending,
                        created_at: Utc::now(),
                    });
                }
            }
            Err(LlmError::Transient { reason, .. }) => {
                tracing::warn!(
                    source_file = %chunk.source_file,
                    source_heading = ?chunk.source_heading,
                    reason = %reason,
                    "llm transient failure",
                );
                failed_chunks += 1;
            }
            Err(LlmError::BadInput { reason }) => {
                tracing::debug!(
                    source_file = %chunk.source_file,
                    source_heading = ?chunk.source_heading,
                    reason = %reason,
                    "llm bad input",
                );
                skipped_chunks += 1;
            }
            Err(LlmError::Config { reason }) => {
                return Err(AppError::Llm(reason));
            }
        }
    }
    state.store.save_pending(&cards).await?;

    Ok(Json(IngestResponse {
        chunks: chunks.len(),
        proposed_cards,
        failed_chunks,
        skipped_chunks,
    }))
}

#[derive(Serialize)]
struct CardsResponse {
    cards: Vec<Card>,
}

/// `GET /cards/pending` — cards awaiting curation.
async fn cards_pending(State(state): State<AppState>) -> Result<Json<CardsResponse>, AppError> {
    Ok(Json(CardsResponse {
        cards: state.store.list_pending().await?,
    }))
}

/// `GET /cards/due` — cards whose `next_due <= now`.
async fn cards_due(State(state): State<AppState>) -> Result<Json<CardsResponse>, AppError> {
    Ok(Json(CardsResponse {
        cards: state.store.list_due(Utc::now()).await?,
    }))
}

/// `POST /cards/{id}/accept` — move a pending card into the SRS pool and seed
/// its initial state, due immediately. Re-accepting an already-accepted card
/// resets its SRS state (acceptable in v1; the curation UI gates this).
async fn accept_card(
    State(state): State<AppState>,
    Path(id): Path<CardId>,
) -> Result<StatusCode, AppError> {
    state.store.set_status(id, CardStatus::Accepted).await?;
    state
        .store
        .save_state(id, SchedulerState::default(), Utc::now())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /cards/{id}/reject` — drop a pending card (flagged, not deleted).
async fn reject_card(
    State(state): State<AppState>,
    Path(id): Path<CardId>,
) -> Result<StatusCode, AppError> {
    state.store.set_status(id, CardStatus::Rejected).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct UpdateContentRequest {
    content: CardContent,
}

/// `PATCH /cards/{id}` — edit a pending card's content (409 if not pending).
async fn patch_card(
    State(state): State<AppState>,
    Path(id): Path<CardId>,
    Json(req): Json<UpdateContentRequest>,
) -> Result<StatusCode, AppError> {
    state.store.update_content(id, req.content).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ReviewRequest {
    card_id: CardId,
    rating: Rating,
}

#[derive(Serialize)]
struct ReviewResponse {
    next_due: DateTime<Utc>,
    interval_days: u32,
}

/// `POST /reviews` — record a review, advance SRS state, persist both.
async fn post_review(
    State(state): State<AppState>,
    Json(req): Json<ReviewRequest>,
) -> Result<Json<ReviewResponse>, AppError> {
    let now = Utc::now();
    let prior = state.store.load_state(req.card_id).await?;
    let outcome = state.scheduler.on_review(prior, req.rating, now);
    state
        .store
        .save_review(&Review {
            card_id: req.card_id,
            reviewed_at: now,
            rating: req.rating,
            next_due: outcome.next_due,
        })
        .await?;
    state
        .store
        .save_state(req.card_id, outcome.state, outcome.next_due)
        .await?;
    Ok(Json(ReviewResponse {
        next_due: outcome.next_due,
        interval_days: outcome.state.interval_days,
    }))
}

/// The note path is untrusted HTTP input and becomes the card's `(source_file,
/// source_heading)` anchor. The store hashes it for the on-disk filename, so
/// this isn't the only traversal defense — but the anchor itself must stay a
/// clean, portable, vault-relative path. Reject absolute paths and `..`.
fn validate_source_file(source_file: &str) -> Result<(), AppError> {
    if source_file.is_empty() {
        return Err(AppError::BadRequest("source_file is empty".into()));
    }
    for comp in std::path::Path::new(source_file).components() {
        if !matches!(comp, Component::Normal(_) | Component::CurDir) {
            return Err(AppError::BadRequest(format!(
                "source_file must be a relative path without '..': {source_file}"
            )));
        }
    }
    Ok(())
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::Io(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".into()),
            AppError::Llm(s) => (StatusCode::INTERNAL_SERVER_ERROR, s),
            AppError::Parse(s) => (StatusCode::BAD_REQUEST, s),
            AppError::BadRequest(s) => (StatusCode::BAD_REQUEST, s),
            AppError::Conflict(s) => (StatusCode::CONFLICT, s),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}
