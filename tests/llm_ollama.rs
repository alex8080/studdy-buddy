//! Live smoke test for the Ollama provider. Hits a real Ollama server (local
//! or cloud-routed via `ollama signin`). `#[ignore]`-d by default and
//! additionally gated by an env var so even `--ignored` runs are explicit.
//!
//! Run with:
//!
//!   STUDYBUDDY_OLLAMA_LIVE=1 \
//!   STUDYBUDDY_OLLAMA_MODEL=gpt-oss:120b-cloud \
//!   cargo test --test llm_ollama -- --ignored
//!
//! Optional: STUDYBUDDY_OLLAMA_BASE_URL (default http://127.0.0.1:11434).

use studybuddy::llm::ollama::{OllamaConfig, OllamaProvider};
use studybuddy::llm::{ChunkContext, LlmProvider};
use studybuddy::model::CardContent;

#[tokio::test]
#[ignore]
async fn ollama_live_smoke() {
    if std::env::var("STUDYBUDDY_OLLAMA_LIVE").as_deref() != Ok("1") {
        eprintln!("skipping: STUDYBUDDY_OLLAMA_LIVE != \"1\"");
        return;
    }
    let model = std::env::var("STUDYBUDDY_OLLAMA_MODEL")
        .expect("STUDYBUDDY_OLLAMA_MODEL must be set, e.g. gpt-oss:120b-cloud");
    let base_url = std::env::var("STUDYBUDDY_OLLAMA_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());

    let provider = OllamaProvider::new(OllamaConfig {
        model,
        base_url,
        temperature: Some(0.2),
        num_predict: None,
    });

    let chunk = ChunkContext {
        source_file: "dot_product.md".to_string(),
        source_heading: Some("Linear Algebra > Vectors > Dot Product".to_string()),
        tags: vec!["math".to_string(), "linear-algebra".to_string()],
        text: "The dot product of two vectors a and b is defined as \
               a · b = |a| |b| cos(θ), where θ is the angle between them. \
               The result is a scalar."
            .to_string(),
    };

    let cards = provider
        .propose_cards(&chunk)
        .await
        .expect("ollama call should succeed");

    assert!(!cards.is_empty(), "expected at least one card");
    for (i, card) in cards.iter().enumerate() {
        match &card.content {
            CardContent::Qa { front, back } => {
                assert!(!front.is_empty(), "card #{i} has empty front");
                assert!(!back.is_empty(), "card #{i} has empty back");
            }
            other => panic!("card #{i} is not Qa: {other:?}"),
        }
    }
}
