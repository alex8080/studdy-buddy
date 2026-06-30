//! HTTP client for the StudyBuddy server.
//!
//! A thin, typed wrapper over `reqwest` — one method per endpoint, sharing the
//! wire types in [`crate::wire`] with the server so the two can't drift. This
//! is the single place anything (the `sb` CLI, the watcher's push) talks to the
//! server; it holds no domain logic.

use anyhow::{Result, anyhow};
use reqwest::{
    Response, StatusCode,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::de::DeserializeOwned;

use crate::model::{Card, CardContent, CardId, Rating};
use crate::wire::{
    CardsResponse, ERROR_KEY, EvaluateRequest, EvaluateResponse, IngestRequest, IngestResponse,
    ReviewRequest, ReviewResponse, UpdateContentRequest, path,
};

/// A handle to a running StudyBuddy server.
pub struct Client {
    base_url: String,
    http: reqwest::Client,
}

impl Client {
    /// Build a client for `base_url` (e.g. `http://127.0.0.1:8080`). A trailing
    /// slash is trimmed so path joins stay clean.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::build(base_url, None)
    }

    /// Build an authenticated client. Every request will carry
    /// `Authorization: Bearer <token>`.
    pub fn authenticated(base_url: impl Into<String>, token: &str) -> Self {
        Self::build(base_url, Some(token))
    }

    fn build(base_url: impl Into<String>, token: Option<&str>) -> Self {
        let mut base = base_url.into();
        while base.ends_with('/') {
            base.pop();
        }
        let http = match token {
            Some(t) => {
                let value = HeaderValue::from_str(&format!("Bearer {t}"))
                    .expect("api token must be a valid HTTP header value");
                let mut headers = HeaderMap::new();
                headers.insert(AUTHORIZATION, value);
                reqwest::Client::builder()
                    .default_headers(headers)
                    .build()
                    .expect("reqwest client build")
            }
            None => reqwest::Client::new(),
        };
        Self {
            base_url: base,
            http,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// `POST /ingest` — push one note's raw markdown for card generation.
    pub async fn ingest(&self, source_file: &str, content: &str) -> Result<IngestResponse> {
        let req = IngestRequest {
            source_file: source_file.to_string(),
            content: content.to_string(),
        };
        let resp = self
            .http
            .post(self.url(path::INGEST))
            .json(&req)
            .send()
            .await?;
        json_or_err(resp).await
    }

    /// `GET /cards/pending` — cards awaiting curation.
    pub async fn pending(&self) -> Result<Vec<Card>> {
        let resp = self.http.get(self.url(path::CARDS_PENDING)).send().await?;
        let body: CardsResponse = json_or_err(resp).await?;
        Ok(body.cards)
    }

    /// `GET /cards/due` — cards whose `next_due <= now`.
    pub async fn due(&self) -> Result<Vec<Card>> {
        let resp = self.http.get(self.url(path::CARDS_DUE)).send().await?;
        let body: CardsResponse = json_or_err(resp).await?;
        Ok(body.cards)
    }

    /// `POST /cards/{id}/accept` — move a pending card into the SRS pool.
    pub async fn accept(&self, id: CardId) -> Result<()> {
        let resp = self
            .http
            .post(self.url(&format!("/cards/{id}/accept")))
            .send()
            .await?;
        empty_or_err(resp).await
    }

    /// `POST /cards/{id}/reject` — drop a pending card.
    pub async fn reject(&self, id: CardId) -> Result<()> {
        let resp = self
            .http
            .post(self.url(&format!("/cards/{id}/reject")))
            .send()
            .await?;
        empty_or_err(resp).await
    }

    /// `PATCH /cards/{id}` — edit a pending card's content (409 if not pending).
    pub async fn patch(&self, id: CardId, content: CardContent) -> Result<()> {
        let req = UpdateContentRequest { content };
        let resp = self
            .http
            .patch(self.url(&format!("/cards/{id}")))
            .json(&req)
            .send()
            .await?;
        empty_or_err(resp).await
    }

    /// `POST /reviews/evaluate` — grade a free-text answer against a card.
    pub async fn evaluate(&self, card_id: CardId, user_answer: &str) -> Result<EvaluateResponse> {
        let req = EvaluateRequest {
            card_id,
            user_answer: user_answer.to_string(),
        };
        let resp = self
            .http
            .post(self.url(path::REVIEWS_EVALUATE))
            .json(&req)
            .send()
            .await?;
        json_or_err(resp).await
    }

    /// `POST /reviews` — record a review and get the next scheduling.
    pub async fn review(&self, card_id: CardId, rating: Rating) -> Result<ReviewResponse> {
        let req = ReviewRequest { card_id, rating };
        let resp = self
            .http
            .post(self.url(path::REVIEWS))
            .json(&req)
            .send()
            .await?;
        json_or_err(resp).await
    }
}

/// Deserialize a successful JSON response, or turn a non-2xx into an error.
async fn json_or_err<T: DeserializeOwned>(resp: Response) -> Result<T> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp.json::<T>().await?)
    } else {
        Err(error_from(status, resp).await)
    }
}

/// Expect an empty (2xx, no body) response, or turn a non-2xx into an error.
async fn empty_or_err(resp: Response) -> Result<()> {
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(error_from(status, resp).await)
    }
}

/// Build an error from a non-success response, surfacing the server's
/// `{ "error": "<message>" }` body when present.
async fn error_from(status: StatusCode, resp: Response) -> anyhow::Error {
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get(ERROR_KEY)
                .and_then(|e| e.as_str())
                .map(str::to_string)
        })
        .filter(|s| !s.is_empty())
        .unwrap_or(body);
    anyhow!("server returned {status}: {msg}")
}

/// Validate an API token before it reaches the HTTP layer.
///
/// HTTP header values must be ASCII; a non-ASCII token would panic inside
/// `Client::authenticated`. Call this at process startup when reading the token
/// from an env var or CLI flag so the user sees a clean error.
pub fn validate_api_token(token: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        token.is_ascii(),
        "STUDYBUDDY_API_TOKEN must contain only ASCII characters"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Verdict;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn ingest_posts_request_and_parses_counts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ingest"))
            .and(body_json(serde_json::json!({
                "source_file": "vectors.md",
                "content": "# Vectors\n"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "chunks": 2,
                "proposed_cards": 3,
                "failed_chunks": 0,
                "skipped_chunks": 1
            })))
            .mount(&server)
            .await;

        let client = Client::new(server.uri());
        let resp = client.ingest("vectors.md", "# Vectors\n").await.unwrap();
        assert_eq!(
            resp,
            IngestResponse {
                chunks: 2,
                proposed_cards: 3,
                failed_chunks: 0,
                skipped_chunks: 1,
            }
        );
    }

    #[tokio::test]
    async fn pending_gets_and_unwraps_cards() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cards/pending"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "cards": [{
                    "id": "00000000-0000-0000-0000-000000000001",
                    "content": { "type": "qa", "front": "Q", "back": "A" },
                    "source_file": "vectors.md",
                    "source_heading": null,
                    "tags": [],
                    "status": "pending",
                    "created_at": "2026-06-22T00:00:00Z"
                }]
            })))
            .mount(&server)
            .await;

        let client = Client::new(server.uri());
        let cards = client.pending().await.unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].source_file, "vectors.md");
    }

    #[tokio::test]
    async fn accept_hits_accept_path_and_accepts_204() {
        let server = MockServer::start().await;
        let id = uuid::Uuid::nil();
        Mock::given(method("POST"))
            .and(path(format!("/cards/{id}/accept")))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let client = Client::new(server.uri());
        client.accept(id).await.unwrap();
    }

    #[tokio::test]
    async fn review_posts_rating_and_parses_next_due() {
        let server = MockServer::start().await;
        let id = uuid::Uuid::nil();
        Mock::given(method("POST"))
            .and(path("/reviews"))
            .and(body_json(serde_json::json!({
                "card_id": id.to_string(),
                "rating": "good"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "next_due": "2026-06-24T00:00:00Z",
                "interval_days": 4
            })))
            .mount(&server)
            .await;

        let client = Client::new(server.uri());
        let resp = client.review(id, Rating::Good).await.unwrap();
        assert_eq!(resp.interval_days, 4);
    }

    #[tokio::test]
    async fn non_success_surfaces_server_error_message() {
        let server = MockServer::start().await;
        let id = uuid::Uuid::nil();
        Mock::given(method("POST"))
            .and(path("/reviews"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(serde_json::json!({ "error": "not found" })),
            )
            .mount(&server)
            .await;

        let client = Client::new(server.uri());
        let err = client.review(id, Rating::Good).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("404"), "{msg}");
        assert!(msg.contains("not found"), "{msg}");
    }

    #[tokio::test]
    async fn evaluate_posts_request_and_parses_correct_verdict() {
        let server = MockServer::start().await;
        let id = uuid::Uuid::nil();
        Mock::given(method("POST"))
            .and(path("/reviews/evaluate"))
            .and(body_json(serde_json::json!({
                "card_id": id.to_string(),
                "user_answer": "four"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "verdict": "correct",
                "explanation": "Matches expected answer.",
                "suggested_rating": "good"
            })))
            .mount(&server)
            .await;

        let client = Client::new(server.uri());
        let resp = client.evaluate(id, "four").await.unwrap();
        assert_eq!(
            resp,
            EvaluateResponse {
                verdict: Verdict::Correct,
                explanation: "Matches expected answer.".to_string(),
                suggested_rating: Rating::Good,
            }
        );
    }

    #[tokio::test]
    async fn evaluate_parses_incorrect_verdict() {
        let server = MockServer::start().await;
        let id = uuid::Uuid::nil();
        Mock::given(method("POST"))
            .and(path("/reviews/evaluate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "verdict": "incorrect",
                "explanation": "Wrong answer.",
                "suggested_rating": "again"
            })))
            .mount(&server)
            .await;

        let client = Client::new(server.uri());
        let resp = client.evaluate(id, "five").await.unwrap();
        assert_eq!(resp.verdict, Verdict::Incorrect);
        assert_eq!(resp.explanation, "Wrong answer.");
    }

    #[test]
    fn ascii_token_is_valid() {
        assert!(validate_api_token("my-secret-token").is_ok());
        assert!(validate_api_token("550e8400-e29b-41d4-a716-446655440000").is_ok());
    }

    #[test]
    fn non_ascii_token_is_rejected() {
        assert!(validate_api_token("tëken").is_err());
        assert!(validate_api_token("秘密").is_err());
    }
}
