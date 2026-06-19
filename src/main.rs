use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

use studybuddy::api::{self, AppState};
use studybuddy::llm::ollama::{OllamaConfig, OllamaProvider};
use studybuddy::scheduler::Sm2;
use studybuddy::store::FileRepository;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let llm_config = OllamaConfig {
        model: "gpt-oss:120b-cloud".to_string(),
        base_url: "http://127.0.0.1:11434".to_string(),
        temperature: Some(0.2),
        num_predict: None,
    };
    let data_dir =
        std::env::var("STUDYBUDDY_DATA_DIR").unwrap_or_else(|_| "./studybuddy-data".to_string());
    let state = AppState {
        llm: Arc::new(OllamaProvider::new(llm_config)),
        store: Arc::new(FileRepository::new(PathBuf::from(data_dir))),
        scheduler: Arc::new(Sm2),
    };

    let addr: SocketAddr = "127.0.0.1:8080".parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("studybuddy listening on http://{addr}");

    axum::serve(listener, api::router(state)).await?;
    Ok(())
}
