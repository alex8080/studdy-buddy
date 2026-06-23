//! Acceptance test for the HTTP `Client` against a real server on an ephemeral
//! port. This is the wire-contract test: it exercises the same `wire` types
//! from both sides over real HTTP, so a server/client drift fails it.

use std::sync::Arc;

use studybuddy::client::Client;
use studybuddy::llm::LlmProvider;
use studybuddy::model::{CardContent, CardStatus, Rating};

mod common;
use common::{FakeLlmProvider, spawn_server};

/// A single-heading note the chunker turns into exactly one chunk → one card.
const NOTE: &str = "# Vectors\n\nA vector has both magnitude and direction. \
Adding two vectors places them tip to tail. The dot product multiplies \
corresponding components and sums them into a scalar.\n";

#[tokio::test]
async fn full_lifecycle_push_curate_review() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let base = spawn_server(llm).await;
    let client = Client::new(base);

    // Push → one chunk, one proposed card.
    let counts = client.ingest("vectors.md", NOTE).await.unwrap();
    assert!(counts.chunks > 0);
    assert_eq!(counts.proposed_cards, counts.chunks);
    assert_eq!(counts.failed_chunks, 0);

    // Pending lists the proposed card; not yet due.
    let pending = client.pending().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, CardStatus::Pending);
    assert_eq!(pending[0].source_file, "vectors.md");
    assert!(client.due().await.unwrap().is_empty());
    let id = pending[0].id;

    // Edit the pending content, then accept it into the SRS pool.
    client
        .patch(
            id,
            CardContent::Qa {
                front: "edited front".to_string(),
                back: "edited back".to_string(),
            },
        )
        .await
        .unwrap();
    client.accept(id).await.unwrap();

    // Accepted → out of pending, now due, carrying the edit.
    assert!(client.pending().await.unwrap().is_empty());
    let due = client.due().await.unwrap();
    assert_eq!(due.len(), 1);
    match &due[0].content {
        CardContent::Qa { front, .. } => assert_eq!(front, "edited front"),
        other => panic!("expected Qa, got {other:?}"),
    }

    // Review (Good) → schedules forward and leaves the due list.
    let outcome = client.review(id, Rating::Good).await.unwrap();
    assert_eq!(outcome.interval_days, 1);
    assert!(client.due().await.unwrap().is_empty());
}

#[tokio::test]
async fn reject_drops_pending_card() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let client = Client::new(spawn_server(llm).await);

    client.ingest("vectors.md", NOTE).await.unwrap();
    let id = client.pending().await.unwrap()[0].id;
    client.reject(id).await.unwrap();
    assert!(client.pending().await.unwrap().is_empty());
}

#[tokio::test]
async fn review_unknown_card_errors() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let client = Client::new(spawn_server(llm).await);

    let err = client
        .review(uuid::Uuid::new_v4(), Rating::Good)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("404"), "{err}");
}
