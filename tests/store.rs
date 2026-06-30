//! Acceptance tests for the file backend, driving the `Repository` trait
//! against a real temp directory. Edge cases of each method live in the
//! in-memory unit tests; here we verify end-to-end behavior and the two things
//! only a real filesystem exercises: persistence across instances and on-disk
//! layout.

use chrono::{Duration, Utc};
use studybuddy::model::{Card, CardContent, CardStatus, Rating, Review};
use studybuddy::scheduler::SchedulerState;
use studybuddy::store::{FileRepository, Repository};
use tempfile::TempDir;
use uuid::Uuid;

fn qa_card(source_file: &str, front: &str, status: CardStatus) -> Card {
    Card {
        id: Uuid::new_v4(),
        content: CardContent::Qa {
            front: front.into(),
            back: "back".into(),
        },
        source_file: source_file.into(),
        source_heading: None,
        tags: vec![],
        status,
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn pending_cards_persist_across_instances() {
    let dir = TempDir::new().unwrap();
    let a = qa_card("a.md", "a", CardStatus::Pending);
    let b = qa_card("b.md", "b", CardStatus::Pending);

    {
        let repo = FileRepository::new(dir.path());
        repo.save_pending(&[a.clone(), b.clone()]).await.unwrap();
    }

    // A fresh instance over the same dir sees the persisted cards.
    let repo = FileRepository::new(dir.path());
    let mut ids: Vec<_> = repo
        .list_pending()
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    ids.sort();
    let mut want = vec![a.id, b.id];
    want.sort();
    assert_eq!(ids, want);
}

#[tokio::test]
async fn sidecar_written_under_cards_with_hashed_name() {
    let dir = TempDir::new().unwrap();
    let repo = FileRepository::new(dir.path());
    repo.save_pending(&[qa_card("topic/sub.md", "q", CardStatus::Pending)])
        .await
        .unwrap();

    // One sidecar under cards/, named by a 64-char sha256 hex stem.
    let entries: Vec<String> = std::fs::read_dir(dir.path().join("cards"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(entries.len(), 1, "expected one sidecar, got {entries:?}");
    let stem = entries[0].strip_suffix(".json").expect("a .json sidecar");
    assert_eq!(stem.len(), 64);
    assert!(stem.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn accept_seeds_state_and_card_becomes_due() {
    let dir = TempDir::new().unwrap();
    let repo = FileRepository::new(dir.path());
    let card = qa_card("c.md", "q", CardStatus::Pending);
    repo.save_pending(std::slice::from_ref(&card))
        .await
        .unwrap();

    // Accept: flip status, seed default state due now.
    let now = Utc::now();
    repo.set_status(card.id, CardStatus::Accepted)
        .await
        .unwrap();
    repo.save_state(card.id, SchedulerState::default(), now)
        .await
        .unwrap();

    assert!(repo.list_pending().await.unwrap().is_empty());
    let due = repo.list_due(now).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, card.id);
    assert_eq!(repo.load_state(card.id).await.unwrap().repetitions, 0);
}

#[tokio::test]
async fn list_due_excludes_future_and_stateless_cards() {
    let dir = TempDir::new().unwrap();
    let repo = FileRepository::new(dir.path());
    let due = qa_card("due.md", "due", CardStatus::Accepted);
    let future = qa_card("future.md", "future", CardStatus::Accepted);
    let no_state = qa_card("nostate.md", "nostate", CardStatus::Accepted);
    repo.save_pending(&[due.clone(), future.clone(), no_state.clone()])
        .await
        .unwrap();

    let now = Utc::now();
    repo.save_state(due.id, SchedulerState::default(), now - Duration::days(1))
        .await
        .unwrap();
    repo.save_state(
        future.id,
        SchedulerState::default(),
        now + Duration::days(1),
    )
    .await
    .unwrap();

    let listed = repo.list_due(now).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, due.id);
}

#[tokio::test]
async fn update_content_persists_and_guards_non_pending() {
    let dir = TempDir::new().unwrap();
    let repo = FileRepository::new(dir.path());
    let card = qa_card("c.md", "old", CardStatus::Pending);
    repo.save_pending(std::slice::from_ref(&card))
        .await
        .unwrap();

    repo.update_content(
        card.id,
        CardContent::Qa {
            front: "new".into(),
            back: "back".into(),
        },
    )
    .await
    .unwrap();

    // Reload from disk: the edit persisted.
    let reloaded = FileRepository::new(dir.path());
    let listed = reloaded.list_pending().await.unwrap();
    match &listed[0].content {
        CardContent::Qa { front, .. } => assert_eq!(front, "new"),
        other => panic!("expected qa, got {other:?}"),
    }

    // Once accepted, content edits are rejected.
    reloaded
        .set_status(card.id, CardStatus::Accepted)
        .await
        .unwrap();
    let err = reloaded
        .update_content(
            card.id,
            CardContent::Qa {
                front: "nope".into(),
                back: "back".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, studybuddy::error::AppError::Conflict(_)));
}

#[tokio::test]
async fn get_card_returns_card_when_present() {
    let dir = TempDir::new().unwrap();
    let repo = FileRepository::new(dir.path());
    let card = qa_card("c.md", "q", CardStatus::Pending);
    repo.save_pending(std::slice::from_ref(&card))
        .await
        .unwrap();
    let got = repo.get_card(card.id).await.unwrap();
    assert_eq!(got.id, card.id);
}

#[tokio::test]
async fn get_card_returns_not_found_for_unknown_id() {
    let dir = TempDir::new().unwrap();
    let repo = FileRepository::new(dir.path());
    let err = repo.get_card(Uuid::new_v4()).await.unwrap_err();
    assert!(matches!(err, studybuddy::error::AppError::NotFound));
}

#[tokio::test]
async fn reviews_append_to_log() {
    let dir = TempDir::new().unwrap();
    let repo = FileRepository::new(dir.path());
    let id = Uuid::new_v4();
    for _ in 0..2 {
        repo.save_review(&Review {
            card_id: id,
            reviewed_at: Utc::now(),
            rating: Rating::Good,
            next_due: Utc::now(),
        })
        .await
        .unwrap();
    }

    let log = std::fs::read_to_string(dir.path().join("reviews.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 2);
}
