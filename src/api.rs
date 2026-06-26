use std::path::Component;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{Next, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::AppError;
use crate::ingest::{ChunkConfig, ingest_text};
use crate::llm::{ChunkContext, LlmError, LlmProvider, ProposedCard};
use crate::model::{Card, CardId, CardStatus, Review};
use crate::scheduler::{Scheduler, SchedulerState};
use crate::store::Repository;
use crate::wire::{
    CardsResponse, ERROR_KEY, IngestRequest, IngestResponse, ReviewRequest, ReviewResponse,
    UpdateContentRequest, path,
};

#[derive(Clone)]
pub struct AppState {
    pub llm: Arc<dyn LlmProvider>,
    pub store: Arc<dyn Repository>,
    pub scheduler: Arc<dyn Scheduler>,
    pub api_token: Option<String>,
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route(path::INGEST, post(ingest))
        .route(path::CARDS_PENDING, get(cards_pending))
        .route(path::CARDS_DUE, get(cards_due))
        .route("/cards/{id}/accept", post(accept_card))
        .route("/cards/{id}/reject", post(reject_card))
        .route("/cards/{id}", patch(patch_card))
        .route(path::REVIEWS, post(post_review))
        .route_layer(from_fn_with_state(state.clone(), require_bearer));

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .with_state(state)
}

async fn require_bearer(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(expected) = &state.api_token else {
        return next.run(req).await;
    };

    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|presented| token_digest(presented) == token_digest(expected))
        .unwrap_or(false);

    if authorized {
        next.run(req).await
    } else {
        let body = Json(serde_json::json!({ ERROR_KEY: "unauthorized" }));
        let mut resp = (StatusCode::UNAUTHORIZED, body).into_response();
        resp.headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        resp
    }
}

fn token_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
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
        match classify_chunk(&*state.llm, chunk).await? {
            ChunkOutcome::Cards(c) => {
                proposed_cards += c.len();
                cards.extend(c);
            }
            ChunkOutcome::Failed => failed_chunks += 1,
            ChunkOutcome::Skipped => skipped_chunks += 1,
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

/// What one chunk contributed to an ingest: cards to persist, a transient
/// failure, or a chunk the LLM declined.
enum ChunkOutcome {
    Cards(Vec<Card>),
    Failed,
    Skipped,
}

/// Propose cards for one chunk and classify the result. Owns the LLM error
/// taxonomy and its logging; a `Config` error is fatal and propagates (aborting
/// the note), while transient/bad-input failures are absorbed per-chunk.
async fn classify_chunk(
    llm: &dyn LlmProvider,
    chunk: &ChunkContext,
) -> Result<ChunkOutcome, AppError> {
    match llm.propose_cards(chunk).await {
        Ok(proposed) => Ok(ChunkOutcome::Cards(
            proposed.into_iter().map(|p| card_from(p, chunk)).collect(),
        )),
        Err(LlmError::Transient { reason, .. }) => {
            tracing::warn!(
                source_file = %chunk.source_file,
                source_heading = ?chunk.source_heading,
                reason = %reason,
                "llm transient failure",
            );
            Ok(ChunkOutcome::Failed)
        }
        Err(LlmError::BadInput { reason }) => {
            tracing::debug!(
                source_file = %chunk.source_file,
                source_heading = ?chunk.source_heading,
                reason = %reason,
                "llm bad input",
            );
            Ok(ChunkOutcome::Skipped)
        }
        Err(LlmError::Config { reason }) => Err(AppError::Llm(reason)),
    }
}

/// Mint a `Pending` card from an LLM proposal, anchored to its source chunk.
fn card_from(proposed: ProposedCard, chunk: &ChunkContext) -> Card {
    Card {
        id: Uuid::new_v4(),
        content: proposed.content,
        source_file: chunk.source_file.clone(),
        source_heading: chunk.source_heading.clone(),
        tags: chunk.tags.clone(),
        status: CardStatus::Pending,
        created_at: Utc::now(),
    }
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

/// `PATCH /cards/{id}` — edit a pending card's content (409 if not pending).
async fn patch_card(
    State(state): State<AppState>,
    Path(id): Path<CardId>,
    Json(req): Json<UpdateContentRequest>,
) -> Result<StatusCode, AppError> {
    state.store.update_content(id, req.content).await?;
    Ok(StatusCode::NO_CONTENT)
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
            AppError::Unauthorized => {
                let body = Json(serde_json::json!({ ERROR_KEY: "unauthorized" }));
                let mut resp = (StatusCode::UNAUTHORIZED, body).into_response();
                resp.headers_mut()
                    .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
                return resp;
            }
        };
        (status, Json(serde_json::json!({ ERROR_KEY: msg }))).into_response()
    }
}
