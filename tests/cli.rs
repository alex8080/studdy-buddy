//! Acceptance tests for the CLI command logic (`studybuddy::cli`), driven with
//! injected I/O against a real spawned server. The thin `bin/sb.rs` clap shell
//! isn't exercised here — these cover everything meaningful below it.

use std::io::Cursor;
use std::sync::Arc;

use studybuddy::cli;
use studybuddy::client::Client;
use studybuddy::llm::{LlmProvider, ProposedCard};
use studybuddy::model::{CardContent, ClozeSpan};

mod common;
use common::{FakeLlmProvider, spawn_server};

const NOTE: &str = "# Vectors\n\nA vector has both magnitude and direction. \
Adding two vectors places them tip to tail. The dot product multiplies \
corresponding components and sums them into a scalar.\n";

/// Ingest `NOTE` then accept the single proposed card, leaving it due.
async fn ingest_and_accept(client: &Client) {
    client.ingest("vectors.md", NOTE).await.unwrap();
    let id = client.pending().await.unwrap()[0].id;
    client.accept(id).await.unwrap();
}

#[tokio::test]
async fn push_sends_note_and_reports_counts() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let client = Client::new(spawn_server(llm).await);

    // A temp vault with a note in a subdirectory.
    let vault = tempfile::TempDir::new().unwrap();
    let note = vault.path().join("linear/vectors.md");
    std::fs::create_dir_all(note.parent().unwrap()).unwrap();
    std::fs::write(&note, NOTE).unwrap();

    let mut out = Vec::new();
    cli::run_push(&client, vault.path(), &note, &mut out)
        .await
        .unwrap();

    let printed = String::from_utf8(out).unwrap();
    // Anchor is vault-relative, forward-slashed; counts reflect one proposed card.
    assert!(printed.contains("linear/vectors.md"), "{printed}");
    assert!(printed.contains("1 proposed"), "{printed}");

    // The push actually landed: the card is pending on the server.
    let pending = client.pending().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source_file, "linear/vectors.md");
}

#[tokio::test]
async fn push_rejects_file_outside_vault() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let client = Client::new(spawn_server(llm).await);

    let vault = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();
    let note = outside.path().join("a.md");
    std::fs::write(&note, NOTE).unwrap();

    let mut out = Vec::new();
    let err = cli::run_push(&client, vault.path(), &note, &mut out)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("outside the vault"), "{err}");
}

#[tokio::test]
async fn review_reveals_then_records_rating() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let client = Client::new(spawn_server(llm).await);
    ingest_and_accept(&client).await;

    // Enter to reveal, then "3" (good).
    let mut input = Cursor::new(b"\n3\n".to_vec());
    let mut out = Vec::new();
    cli::run_review(&client, &mut input, &mut out)
        .await
        .unwrap();

    let printed = String::from_utf8(out).unwrap();
    assert!(printed.contains('Q'), "shows the question: {printed}");
    assert!(printed.contains('A'), "reveals the answer: {printed}");
    assert!(printed.contains("next due in"), "{printed}");

    // The review landed: the card scheduled forward, off the due list.
    assert!(client.due().await.unwrap().is_empty());
}

#[tokio::test]
async fn review_with_nothing_due_says_so() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let client = Client::new(spawn_server(llm).await);

    let mut input = Cursor::new(Vec::new());
    let mut out = Vec::new();
    cli::run_review(&client, &mut input, &mut out)
        .await
        .unwrap();
    assert_eq!(String::from_utf8(out).unwrap().trim(), "nothing due");
}

#[tokio::test]
async fn review_blanks_cloze_question_then_fills_on_reveal() {
    // A fake LLM that proposes a single cloze card.
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::new(|_| {
        let text = "The dot product yields a scalar.".to_string();
        let start = text.find("scalar").unwrap();
        Ok(vec![ProposedCard {
            content: CardContent::Cloze {
                text,
                spans: vec![ClozeSpan {
                    start,
                    end: start + "scalar".len(),
                    hint: None,
                }],
            },
            rationale: None,
        }])
    }));
    let client = Client::new(spawn_server(llm).await);
    ingest_and_accept(&client).await;

    let mut input = Cursor::new(b"\n3\n".to_vec());
    let mut out = Vec::new();
    cli::run_review(&client, &mut input, &mut out)
        .await
        .unwrap();

    let printed = String::from_utf8(out).unwrap();
    assert!(
        printed.contains("yields a {{...}}"),
        "blanked question: {printed}"
    );
    assert!(
        printed.contains("yields a scalar."),
        "filled answer on reveal: {printed}"
    );
}

#[tokio::test]
async fn curate_accept_moves_card_into_due_pool() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let client = Client::new(spawn_server(llm).await);
    client.ingest("vectors.md", NOTE).await.unwrap();

    let mut input = Cursor::new(b"a\n".to_vec());
    let mut out = Vec::new();
    let no_edit = |c: &CardContent| Ok(c.clone());
    cli::run_curate(&client, &mut input, &mut out, no_edit)
        .await
        .unwrap();

    assert!(client.pending().await.unwrap().is_empty());
    // Accepted cards are due immediately.
    assert_eq!(client.due().await.unwrap().len(), 1);
}

#[tokio::test]
async fn curate_reject_drops_card() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let client = Client::new(spawn_server(llm).await);
    client.ingest("vectors.md", NOTE).await.unwrap();

    let mut input = Cursor::new(b"r\n".to_vec());
    let mut out = Vec::new();
    let no_edit = |c: &CardContent| Ok(c.clone());
    cli::run_curate(&client, &mut input, &mut out, no_edit)
        .await
        .unwrap();

    assert!(client.pending().await.unwrap().is_empty());
    assert!(client.due().await.unwrap().is_empty());
}

#[tokio::test]
async fn curate_edit_then_accept_persists_edit() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlmProvider::always_one_card());
    let client = Client::new(spawn_server(llm).await);
    client.ingest("vectors.md", NOTE).await.unwrap();

    // Edit, then accept — the editor closure rewrites the content.
    let mut input = Cursor::new(b"e\na\n".to_vec());
    let mut out = Vec::new();
    let edit = |_c: &CardContent| {
        Ok(CardContent::Qa {
            front: "edited front".to_string(),
            back: "edited back".to_string(),
        })
    };
    cli::run_curate(&client, &mut input, &mut out, edit)
        .await
        .unwrap();

    let due = client.due().await.unwrap();
    assert_eq!(due.len(), 1);
    match &due[0].content {
        CardContent::Qa { front, .. } => assert_eq!(front, "edited front"),
        other => panic!("expected Qa, got {other:?}"),
    }
}
