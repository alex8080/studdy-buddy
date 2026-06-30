//! In-memory [`Repository`] — the test double, and what handlers bind to until
//! the file backend lands.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::{AppError, Result};
use crate::model::{Card, CardContent, CardId, CardStatus, Review};
use crate::scheduler::SchedulerState;

use super::Repository;

/// In-memory [`Repository`]. The test double, and what handlers bind to until
/// the file backend lands. Not persistent — state is dropped on process exit.
#[derive(Default)]
pub struct InMemoryRepository {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    cards: HashMap<CardId, Card>,
    /// Mirrors `state.json`: each card's SRS state plus its current `next_due`
    /// (decision B — `next_due` lives here so it is the due-index).
    state: HashMap<CardId, (SchedulerState, DateTime<Utc>)>,
    /// Mirrors the append-only `reviews.jsonl`.
    reviews: Vec<Review>,
}

impl InMemoryRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Repository for InMemoryRepository {
    async fn save_pending(&self, cards: &[Card]) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        for card in cards {
            inner.cards.insert(card.id, card.clone());
        }
        Ok(())
    }

    async fn list_pending(&self) -> Result<Vec<Card>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .cards
            .values()
            .filter(|c| c.status == CardStatus::Pending)
            .cloned()
            .collect())
    }

    async fn update_content(&self, card: CardId, content: CardContent) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let c = inner.cards.get_mut(&card).ok_or(AppError::NotFound)?;
        if c.status != CardStatus::Pending {
            return Err(super::not_pending_err(card));
        }
        c.content = content;
        Ok(())
    }

    async fn set_status(&self, card: CardId, status: CardStatus) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let c = inner.cards.get_mut(&card).ok_or(AppError::NotFound)?;
        c.status = status;
        Ok(())
    }

    async fn list_due(&self, now: DateTime<Utc>) -> Result<Vec<Card>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .state
            .iter()
            .filter(|(_, (_, next_due))| *next_due <= now)
            .filter_map(|(id, _)| inner.cards.get(id).cloned())
            // Only accepted cards are due: a card rejected after acceptance may
            // still have a lingering state entry, but it isn't in the SRS pool.
            .filter(|c| c.status == CardStatus::Accepted)
            .collect())
    }

    async fn load_state(&self, card: CardId) -> Result<SchedulerState> {
        let inner = self.inner.lock().unwrap();
        inner
            .state
            .get(&card)
            .map(|(state, _)| *state)
            .ok_or(AppError::NotFound)
    }

    async fn save_state(
        &self,
        card: CardId,
        state: SchedulerState,
        next_due: DateTime<Utc>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.state.insert(card, (state, next_due));
        Ok(())
    }

    async fn save_review(&self, review: &Review) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.reviews.push(review.clone());
        Ok(())
    }

    async fn get_card(&self, card: CardId) -> Result<Card> {
        let inner = self.inner.lock().unwrap();
        inner.cards.get(&card).cloned().ok_or(AppError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rating;
    use chrono::Duration;
    use uuid::Uuid;

    fn qa_card(front: &str, status: CardStatus) -> Card {
        Card {
            id: Uuid::new_v4(),
            content: CardContent::Qa {
                front: front.into(),
                back: "back".into(),
            },
            source_file: "notes.md".into(),
            source_heading: None,
            tags: vec![],
            status,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn save_pending_then_list_pending_roundtrips() {
        let repo = InMemoryRepository::new();
        let a = qa_card("a", CardStatus::Pending);
        let b = qa_card("b", CardStatus::Pending);
        repo.save_pending(&[a.clone(), b.clone()]).await.unwrap();

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
    async fn list_pending_excludes_non_pending() {
        let repo = InMemoryRepository::new();
        let pending = qa_card("p", CardStatus::Pending);
        let accepted = qa_card("a", CardStatus::Accepted);
        repo.save_pending(&[pending.clone(), accepted.clone()])
            .await
            .unwrap();

        let listed = repo.list_pending().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, pending.id);
    }

    #[tokio::test]
    async fn update_content_edits_pending_card() {
        let repo = InMemoryRepository::new();
        let card = qa_card("old", CardStatus::Pending);
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

        let listed = repo.list_pending().await.unwrap();
        match &listed[0].content {
            CardContent::Qa { front, .. } => assert_eq!(front, "new"),
            other => panic!("expected qa, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_content_rejects_non_pending() {
        let repo = InMemoryRepository::new();
        let card = qa_card("x", CardStatus::Accepted);
        repo.save_pending(std::slice::from_ref(&card))
            .await
            .unwrap();

        let err = repo
            .update_content(
                card.id,
                CardContent::Qa {
                    front: "new".into(),
                    back: "back".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
    }

    #[tokio::test]
    async fn update_content_unknown_card_is_not_found() {
        let repo = InMemoryRepository::new();
        let err = repo
            .update_content(
                Uuid::new_v4(),
                CardContent::Qa {
                    front: "new".into(),
                    back: "back".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound));
    }

    #[tokio::test]
    async fn set_status_moves_card_out_of_pending() {
        let repo = InMemoryRepository::new();
        let card = qa_card("p", CardStatus::Pending);
        repo.save_pending(std::slice::from_ref(&card))
            .await
            .unwrap();

        repo.set_status(card.id, CardStatus::Accepted)
            .await
            .unwrap();
        assert!(repo.list_pending().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn save_state_then_load_state_roundtrips() {
        let repo = InMemoryRepository::new();
        let id = Uuid::new_v4();
        let state = SchedulerState {
            interval_days: 6,
            ease: 2.5,
            repetitions: 2,
        };
        repo.save_state(id, state, Utc::now()).await.unwrap();

        let loaded = repo.load_state(id).await.unwrap();
        assert_eq!(loaded.interval_days, 6);
        assert_eq!(loaded.repetitions, 2);
    }

    #[tokio::test]
    async fn load_state_unknown_card_is_not_found() {
        let repo = InMemoryRepository::new();
        let err = repo.load_state(Uuid::new_v4()).await.unwrap_err();
        assert!(matches!(err, AppError::NotFound));
    }

    #[tokio::test]
    async fn list_due_returns_only_cards_due_at_or_before_now() {
        let repo = InMemoryRepository::new();
        let due = qa_card("due", CardStatus::Accepted);
        let future = qa_card("future", CardStatus::Accepted);
        let no_state = qa_card("no_state", CardStatus::Accepted);
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
    async fn get_card_returns_saved_card() {
        let repo = InMemoryRepository::new();
        let card = qa_card("q", CardStatus::Pending);
        repo.save_pending(std::slice::from_ref(&card))
            .await
            .unwrap();
        let got = repo.get_card(card.id).await.unwrap();
        assert_eq!(got.id, card.id);
    }

    #[tokio::test]
    async fn get_card_returns_not_found_for_unknown_id() {
        let repo = InMemoryRepository::new();
        let err = repo.get_card(Uuid::new_v4()).await.unwrap_err();
        assert!(matches!(err, AppError::NotFound));
    }

    #[tokio::test]
    async fn save_review_appends_to_log() {
        let repo = InMemoryRepository::new();
        let review = Review {
            card_id: Uuid::new_v4(),
            reviewed_at: Utc::now(),
            rating: Rating::Good,
            next_due: Utc::now(),
        };
        repo.save_review(&review).await.unwrap();

        let inner = repo.inner.lock().unwrap();
        assert_eq!(inner.reviews.len(), 1);
        assert_eq!(inner.reviews[0].card_id, review.card_id);
    }
}
