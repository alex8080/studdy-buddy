use std::sync::Arc;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

use studybuddy::api::{self, AppState};
use studybuddy::config::Config;
use studybuddy::llm::LlmProvider;
use studybuddy::llm::ollama::{OllamaConfig, OllamaProvider};
use studybuddy::llm::retry::{RetryPolicy, RetryingProvider};
use studybuddy::scheduler::Sm2;
use studybuddy::store::FileRepository;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::load()?;

    let llm: Arc<dyn LlmProvider> = match config.llm.provider.as_str() {
        "ollama" => {
            let ollama_config = OllamaConfig {
                model: config.llm.model,
                base_url: config.llm.base_url,
                temperature: config.llm.temperature,
                num_predict: config.llm.num_predict,
            };
            Arc::new(RetryingProvider::new(
                OllamaProvider::new(ollama_config),
                RetryPolicy::default(),
            ))
        }
        other => anyhow::bail!(
            "unsupported LLM provider '{other}' — only 'ollama' is supported in this build"
        ),
    };

    let state = AppState {
        llm,
        store: Arc::new(FileRepository::new(config.store.data_dir)),
        scheduler: Arc::new(Sm2),
        api_token: config.server.api_token,
    };

    let listener = tokio::net::TcpListener::bind(config.server.bind).await?;
    tracing::info!("studybuddy listening on http://{}", config.server.bind);

    axum::serve(listener, api::router(state)).await?;
    Ok(())
}
