//! Shared test harness for the integration-test crates (`api`, `client`, `cli`).
//!
//! Holds the `FakeLlmProvider` double and two ways to stand up the server:
//! `in_memory_router` for `tower::oneshot` tests (no socket) and `spawn_server`
//! for tests that need a real listening port (the HTTP client makes real
//! requests). Not every test crate uses every helper, hence `allow(dead_code)`.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::Router;
use studybuddy::api::{self, AppState};
use studybuddy::llm::{ChunkContext, LlmError, LlmProvider, ProposedCard};
use studybuddy::model::CardContent;
use studybuddy::scheduler::Sm2;
use studybuddy::store::{InMemoryRepository, Repository};

type Respond = Box<dyn Fn(&ChunkContext) -> Result<Vec<ProposedCard>, LlmError> + Send + Sync>;

/// A scriptable `LlmProvider` double: returns whatever its closure says and
/// counts how many times it was asked to propose cards.
pub struct FakeLlmProvider {
    respond: Respond,
    calls: AtomicUsize,
}

impl FakeLlmProvider {
    pub fn new(
        respond: impl Fn(&ChunkContext) -> Result<Vec<ProposedCard>, LlmError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            respond: Box::new(respond),
            calls: AtomicUsize::new(0),
        }
    }

    /// Always proposes a single `Q`/`A` card per chunk.
    pub fn always_one_card() -> Self {
        Self::new(|_| {
            Ok(vec![ProposedCard {
                content: CardContent::Qa {
                    front: "Q".to_string(),
                    back: "A".to_string(),
                },
                rationale: None,
            }])
        })
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmProvider for FakeLlmProvider {
    async fn propose_cards(&self, chunk: &ChunkContext) -> Result<Vec<ProposedCard>, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        (self.respond)(chunk)
    }
}

/// Build the axum router over an in-memory store + SM-2 scheduler.
pub fn in_memory_router(llm: Arc<dyn LlmProvider>) -> Router {
    let store: Arc<dyn Repository> = Arc::new(InMemoryRepository::new());
    api::router(AppState {
        llm,
        store,
        scheduler: Arc::new(Sm2),
    })
}

/// Bind an ephemeral loopback port, serve the in-memory app on a background
/// task, and return its base URL (e.g. `http://127.0.0.1:54321`). The port is
/// read from `local_addr()` before serving so the URL is ready on return.
pub async fn spawn_server(llm: Arc<dyn LlmProvider>) -> String {
    let app = in_memory_router(llm);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}
