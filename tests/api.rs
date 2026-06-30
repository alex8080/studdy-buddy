//! End-to-end tests against the axum router. Uses `tower::ServiceExt::oneshot`
//! to drive the router directly without binding a socket.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use studybuddy::api::{self, AppState};
use studybuddy::llm::{EvaluationResult, LlmError, LlmProvider, ProposedCard};
use studybuddy::model::{Card, CardContent, ClozeSpan, Verdict};
use studybuddy::scheduler::Sm2;
use studybuddy::store::{FileRepository, Repository};
use tower::ServiceExt;
use uuid::Uuid;

mod common;
use common::{FakeLlmProvider, in_memory_router as router, in_memory_router_with_token};

/// A single-heading note that the chunker turns into exactly one chunk.
const NOTE: &str = "# Vectors\n\nA vector has both magnitude and direction. \
Adding two vectors places them tip to tail. The dot product multiplies \
corresponding components and sums them into a scalar.\n";

/// Same note, but frontmatter-excluded — `ingest_text` yields no chunks.
const EXCLUDED_NOTE: &str =
    "---\nstudybuddy:\n  exclude: true\n---\n\n# Vectors\n\nbody text here.\n";

async fn post_ingest(
    llm: Arc<dyn LlmProvider>,
    source_file: &str,
    content: &str,
) -> (StatusCode, serde_json::Value) {
    let body = serde_json::json!({ "source_file": source_file, "content": content }).to_string();
    let res = router(llm)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ingest")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let v: serde_json::Value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, v)
}

#[tokio::test]
async fn health_returns_ok_json() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let res = router(llm)
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["status"], "ok");
}

#[tokio::test]
async fn ingest_returns_counts_for_pushed_note() {
    let fake = Arc::new(FakeLlmProvider::always_one_card());
    let llm: Arc<dyn LlmProvider> = fake.clone();
    let (status, body) = post_ingest(llm, "vectors.md", NOTE).await;
    assert_eq!(status, StatusCode::OK);

    let chunks = body["chunks"].as_u64().unwrap() as usize;
    assert!(chunks > 0);
    assert_eq!(body["proposed_cards"].as_u64().unwrap() as usize, chunks);
    assert_eq!(body["failed_chunks"].as_u64().unwrap(), 0);
    assert_eq!(body["skipped_chunks"].as_u64().unwrap(), 0);
    assert_eq!(fake.calls(), chunks, "LLM called once per chunk");
}

#[tokio::test]
async fn ingest_excluded_note_yields_zero_chunks() {
    let fake = Arc::new(FakeLlmProvider::always_one_card());
    let llm: Arc<dyn LlmProvider> = fake.clone();
    let (status, body) = post_ingest(llm, "ignored.md", EXCLUDED_NOTE).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["chunks"].as_u64().unwrap(), 0);
    assert_eq!(body["proposed_cards"].as_u64().unwrap(), 0);
    assert_eq!(fake.calls(), 0, "excluded note contributes no LLM calls");
}

#[tokio::test]
async fn ingest_counts_bad_input_as_skipped_not_failed() {
    let fake = Arc::new(FakeLlmProvider::new(|_| {
        Err(LlmError::BadInput {
            reason: "model said no".to_string(),
        })
    }));
    let llm: Arc<dyn LlmProvider> = fake.clone();
    let (status, body) = post_ingest(llm, "vectors.md", NOTE).await;
    assert_eq!(status, StatusCode::OK);

    let chunks = body["chunks"].as_u64().unwrap() as usize;
    assert!(chunks > 0);
    assert_eq!(body["proposed_cards"].as_u64().unwrap(), 0);
    assert_eq!(body["failed_chunks"].as_u64().unwrap(), 0);
    assert_eq!(body["skipped_chunks"].as_u64().unwrap() as usize, chunks);
}

#[tokio::test]
async fn ingest_counts_transient_as_failed() {
    let fake = Arc::new(FakeLlmProvider::new(|_| {
        Err(LlmError::Transient {
            reason: "timeout".to_string(),
            retry_after: None,
        })
    }));
    let llm: Arc<dyn LlmProvider> = fake.clone();
    let (status, body) = post_ingest(llm, "vectors.md", NOTE).await;
    assert_eq!(status, StatusCode::OK);

    let chunks = body["chunks"].as_u64().unwrap() as usize;
    assert!(chunks > 0);
    assert_eq!(body["proposed_cards"].as_u64().unwrap(), 0);
    assert_eq!(body["skipped_chunks"].as_u64().unwrap(), 0);
    assert_eq!(body["failed_chunks"].as_u64().unwrap() as usize, chunks);
}

#[tokio::test]
async fn ingest_aborts_on_config_error() {
    let fake = Arc::new(FakeLlmProvider::new(|_| {
        Err(LlmError::Config {
            reason: "unknown model".to_string(),
        })
    }));
    let llm: Arc<dyn LlmProvider> = fake.clone();
    let (status, body) = post_ingest(llm, "vectors.md", NOTE).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body["error"].as_str().unwrap().contains("unknown model"));
    assert_eq!(fake.calls(), 1, "must stop on the first Config error");
}

#[tokio::test]
async fn ingest_handles_empty_card_proposals() {
    let fake = Arc::new(FakeLlmProvider::new(|_| Ok(vec![])));
    let llm: Arc<dyn LlmProvider> = fake.clone();
    let (status, body) = post_ingest(llm, "vectors.md", NOTE).await;
    assert_eq!(status, StatusCode::OK);

    let chunks = body["chunks"].as_u64().unwrap() as usize;
    assert!(chunks > 0);
    assert_eq!(body["proposed_cards"].as_u64().unwrap(), 0);
    assert_eq!(body["failed_chunks"].as_u64().unwrap(), 0);
    assert_eq!(body["skipped_chunks"].as_u64().unwrap(), 0);
}

#[tokio::test]
async fn ingest_rejects_absolute_source_file() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let (status, body) = post_ingest(llm, "/etc/passwd", NOTE).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn ingest_rejects_parent_traversal() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let (status, body) = post_ingest(llm, "../secret.md", NOTE).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].is_string());
}

// ---- curation + review (shared store across requests) ----
//
// `Router` clones its `Arc`'d `AppState`, so building the app once and cloning
// it per `oneshot` means every request hits the same in-memory store.

/// Send one request against a shared app. A `Null` body becomes an empty body
/// (for GET / no-body POSTs); anything else is sent as JSON.
async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req_body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(req_body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let v: serde_json::Value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, v)
}

fn app_with_one_card() -> axum::Router {
    router(Arc::new(FakeLlmProvider::always_one_card()))
}

async fn ingest_one_pending(app: &axum::Router, source_file: &str) -> String {
    let (status, _) = send(
        app,
        "POST",
        "/ingest",
        serde_json::json!({ "source_file": source_file, "content": NOTE }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = send(app, "GET", "/cards/pending", serde_json::Value::Null).await;
    let cards = body["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 1, "expected one pending card");
    cards[0]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn full_lifecycle_ingest_curate_review() {
    let app = app_with_one_card();
    let id = ingest_one_pending(&app, "vectors.md").await;

    // Pending → not due yet (no SRS state).
    let (_, due) = send(&app, "GET", "/cards/due", serde_json::Value::Null).await;
    assert!(due["cards"].as_array().unwrap().is_empty());

    // Accept → leaves pending, due immediately.
    let (status, _) = send(
        &app,
        "POST",
        &format!("/cards/{id}/accept"),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, pending) = send(&app, "GET", "/cards/pending", serde_json::Value::Null).await;
    assert!(pending["cards"].as_array().unwrap().is_empty());
    let (_, due) = send(&app, "GET", "/cards/due", serde_json::Value::Null).await;
    assert_eq!(due["cards"].as_array().unwrap().len(), 1);

    // Review (Good) → schedules forward, leaves the due list.
    let (status, review) = send(
        &app,
        "POST",
        "/reviews",
        serde_json::json!({ "card_id": id, "rating": "good" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(review["interval_days"].as_u64().unwrap(), 1);
    let (_, due) = send(&app, "GET", "/cards/due", serde_json::Value::Null).await;
    assert!(
        due["cards"].as_array().unwrap().is_empty(),
        "reviewed card scheduled into the future"
    );
}

#[tokio::test]
async fn patch_edits_pending_then_conflicts_after_accept() {
    let app = app_with_one_card();
    let id = ingest_one_pending(&app, "vectors.md").await;

    // Edit the pending card's content.
    let (status, _) = send(
        &app,
        "PATCH",
        &format!("/cards/{id}"),
        serde_json::json!({ "content": { "type": "qa", "front": "new front", "back": "new back" } }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, pending) = send(&app, "GET", "/cards/pending", serde_json::Value::Null).await;
    assert_eq!(pending["cards"][0]["content"]["front"], "new front");

    // After accept, content edits are rejected with 409.
    send(
        &app,
        "POST",
        &format!("/cards/{id}/accept"),
        serde_json::Value::Null,
    )
    .await;
    let (status, _) = send(
        &app,
        "PATCH",
        &format!("/cards/{id}"),
        serde_json::json!({ "content": { "type": "qa", "front": "x", "back": "y" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn reject_removes_card_from_pending() {
    let app = app_with_one_card();
    let id = ingest_one_pending(&app, "vectors.md").await;

    let (status, _) = send(
        &app,
        "POST",
        &format!("/cards/{id}/reject"),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, pending) = send(&app, "GET", "/cards/pending", serde_json::Value::Null).await;
    assert!(pending["cards"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn reject_after_accept_removes_card_from_due() {
    let app = app_with_one_card();
    let id = ingest_one_pending(&app, "vectors.md").await;

    // Accept (now due), then reject — it must not resurface as due.
    send(
        &app,
        "POST",
        &format!("/cards/{id}/accept"),
        serde_json::Value::Null,
    )
    .await;
    let (_, due) = send(&app, "GET", "/cards/due", serde_json::Value::Null).await;
    assert_eq!(due["cards"].as_array().unwrap().len(), 1);

    let (status, _) = send(
        &app,
        "POST",
        &format!("/cards/{id}/reject"),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, due) = send(&app, "GET", "/cards/due", serde_json::Value::Null).await;
    assert!(
        due["cards"].as_array().unwrap().is_empty(),
        "rejected card must not stay due"
    );
}

#[tokio::test]
async fn file_backed_lifecycle_smoke() {
    // Exercises the real production path — the HTTP layer over `FileRepository`
    // (not the in-memory double) — end to end against a tempdir. `dir` must
    // outlive the requests, so it's held for the whole test.
    let dir = tempfile::TempDir::new().unwrap();
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let store: Arc<dyn Repository> = Arc::new(FileRepository::new(dir.path()));
    let app = api::router(AppState {
        llm,
        store,
        scheduler: Arc::new(Sm2),
        api_token: None,
    });

    // ingest → pending → accept → due → review, all hitting the file store.
    let id = ingest_one_pending(&app, "vectors.md").await;
    let (status, _) = send(
        &app,
        "POST",
        &format!("/cards/{id}/accept"),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, due) = send(&app, "GET", "/cards/due", serde_json::Value::Null).await;
    assert_eq!(due["cards"].as_array().unwrap().len(), 1);
    let (status, review) = send(
        &app,
        "POST",
        "/reviews",
        serde_json::json!({ "card_id": id, "rating": "good" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(review["interval_days"].as_u64().unwrap(), 1);

    // The store actually wrote to disk.
    assert!(dir.path().join("cards").is_dir(), "card sidecars written");
    assert!(dir.path().join("state.json").is_file(), "SRS state written");
    assert!(
        dir.path().join("reviews.jsonl").is_file(),
        "review log written"
    );
}

#[tokio::test]
async fn review_unknown_card_is_404() {
    let app = app_with_one_card();
    let (status, _) = send(
        &app,
        "POST",
        "/reviews",
        serde_json::json!({ "card_id": Uuid::new_v4().to_string(), "rating": "good" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---- bearer token auth ----

const TEST_TOKEN: &str = "s3cr3t";

fn app_with_token() -> axum::Router {
    in_memory_router_with_token(
        Arc::new(FakeLlmProvider::always_one_card()),
        Some(TEST_TOKEN.to_string()),
    )
}

async fn oneshot(app: axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let v: serde_json::Value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, v)
}

#[tokio::test]
async fn health_is_open_when_token_configured() {
    let (status, body) = oneshot(
        app_with_token(),
        Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn protected_endpoint_is_401_without_token_header() {
    let (status, body) = oneshot(
        app_with_token(),
        Request::builder()
            .method("GET")
            .uri("/cards/pending")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body["error"].as_str().is_some());
}

#[tokio::test]
async fn protected_endpoint_is_401_with_wrong_token() {
    let (status, body) = oneshot(
        app_with_token(),
        Request::builder()
            .method("GET")
            .uri("/cards/pending")
            .header("authorization", "Bearer wrong-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body["error"].as_str().is_some());
}

#[tokio::test]
async fn protected_endpoint_is_accessible_with_valid_token() {
    let (status, body) = oneshot(
        app_with_token(),
        Request::builder()
            .method("GET")
            .uri("/cards/pending")
            .header("authorization", format!("Bearer {TEST_TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["cards"].is_array());
}

#[tokio::test]
async fn unauthorized_response_includes_www_authenticate_header() {
    let res = app_with_token()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/cards/pending")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(res.headers().get("www-authenticate").unwrap(), "Bearer");
}

// ---- POST /reviews/evaluate ----

/// Build an app whose LLM always proposes one Q&A card and returns the given
/// evaluate closure when grading answers.
fn app_with_evaluate(
    evaluate: impl Fn(&Card, &str) -> Result<EvaluationResult, LlmError> + Send + Sync + 'static,
) -> axum::Router {
    router(Arc::new(FakeLlmProvider::with_evaluate(
        |_| {
            Ok(vec![ProposedCard {
                content: CardContent::Qa {
                    front: "Q".into(),
                    back: "A".into(),
                },
                rationale: None,
            }])
        },
        evaluate,
    )))
}

/// Build an app whose LLM proposes a single-span cloze card with a known fill.
fn cloze_app() -> axum::Router {
    router(Arc::new(FakeLlmProvider::new(|_| {
        // "Paris" occupies bytes 25..30 in the text below.
        Ok(vec![ProposedCard {
            content: CardContent::Cloze {
                text: "The capital of France is Paris.".into(),
                spans: vec![ClozeSpan {
                    start: 25,
                    end: 30,
                    hint: None,
                }],
            },
            rationale: None,
        }])
    })))
}

#[tokio::test]
async fn evaluate_unknown_card_is_404() {
    let app = app_with_one_card();
    let (status, _) = send(
        &app,
        "POST",
        "/reviews/evaluate",
        serde_json::json!({ "card_id": Uuid::new_v4().to_string(), "user_answer": "anything" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn evaluate_qa_correct_verdict_returns_good_rating() {
    let app = app_with_evaluate(|_, _| {
        Ok(EvaluationResult {
            verdict: Verdict::Correct,
            explanation: "Correct.".into(),
        })
    });
    let id = ingest_one_pending(&app, "test.md").await;
    let (status, body) = send(
        &app,
        "POST",
        "/reviews/evaluate",
        serde_json::json!({ "card_id": id, "user_answer": "A" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["verdict"], "correct");
    assert_eq!(body["suggested_rating"], "good");
}

#[tokio::test]
async fn evaluate_qa_incorrect_verdict_returns_again_rating() {
    let app = app_with_evaluate(|_, _| {
        Ok(EvaluationResult {
            verdict: Verdict::Incorrect,
            explanation: "Wrong.".into(),
        })
    });
    let id = ingest_one_pending(&app, "test.md").await;
    let (status, body) = send(
        &app,
        "POST",
        "/reviews/evaluate",
        serde_json::json!({ "card_id": id, "user_answer": "wrong" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["verdict"], "incorrect");
    assert_eq!(body["suggested_rating"], "again");
}

#[tokio::test]
async fn evaluate_qa_partial_verdict_returns_hard_rating() {
    let app = app_with_evaluate(|_, _| {
        Ok(EvaluationResult {
            verdict: Verdict::Partial,
            explanation: "Partial.".into(),
        })
    });
    let id = ingest_one_pending(&app, "test.md").await;
    let (status, body) = send(
        &app,
        "POST",
        "/reviews/evaluate",
        serde_json::json!({ "card_id": id, "user_answer": "partial" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["verdict"], "partial");
    assert_eq!(body["suggested_rating"], "hard");
}

#[tokio::test]
async fn evaluate_qa_llm_transient_failure_is_503() {
    let app = app_with_evaluate(|_, _| {
        Err(LlmError::Transient {
            reason: "timeout".into(),
            retry_after: None,
        })
    });
    let id = ingest_one_pending(&app, "test.md").await;
    let (status, _) = send(
        &app,
        "POST",
        "/reviews/evaluate",
        serde_json::json!({ "card_id": id, "user_answer": "anything" }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn evaluate_cloze_correct_answer_is_correct() {
    let app = cloze_app();
    let id = ingest_one_pending(&app, "test.md").await;
    let (status, body) = send(
        &app,
        "POST",
        "/reviews/evaluate",
        serde_json::json!({ "card_id": id, "user_answer": "Paris" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["verdict"], "correct");
    assert_eq!(body["suggested_rating"], "good");
}

#[tokio::test]
async fn evaluate_cloze_wrong_answer_is_incorrect() {
    let app = cloze_app();
    let id = ingest_one_pending(&app, "test.md").await;
    let (status, body) = send(
        &app,
        "POST",
        "/reviews/evaluate",
        serde_json::json!({ "card_id": id, "user_answer": "London" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["verdict"], "incorrect");
    assert_eq!(body["suggested_rating"], "again");
}

#[tokio::test]
async fn evaluate_cloze_case_insensitive_match_is_correct() {
    let app = cloze_app();
    let id = ingest_one_pending(&app, "test.md").await;
    let (status, body) = send(
        &app,
        "POST",
        "/reviews/evaluate",
        serde_json::json!({ "card_id": id, "user_answer": "PARIS" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["verdict"], "correct");
}
