use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::prompt;
use super::{ChunkContext, LlmError, LlmProvider, ProposedCard};
use crate::model::CardContent;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub struct OllamaProvider {
    config: OllamaConfig,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(config: OllamaConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client build");
        Self { config, client }
    }
}

#[derive(Debug)]
pub struct OllamaConfig {
    pub model: String,
    pub base_url: String,
    pub temperature: Option<f32>,
    pub num_predict: Option<u32>,
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    async fn propose_cards(&self, chunk: &ChunkContext) -> Result<Vec<ProposedCard>, LlmError> {
        let user = prompt::render_user(chunk);
        let request = ChatRequest {
            model: &self.config.model,
            messages: vec![
                Message {
                    role: "system",
                    content: prompt::SYSTEM_PROMPT,
                },
                Message {
                    role: "user",
                    content: &user,
                },
            ],
            stream: false,
            format: response_schema(),
            options: ChatOptions {
                temperature: self.config.temperature,
                num_predict: self.config.num_predict,
            },
        };

        let url = format!("{}/api/chat", self.config.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| LlmError::Transient {
                reason: format!("ollama request failed: {e}"),
                retry_after: None,
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::Config {
                reason: format!("ollama returned {status}: {body}"),
            });
        }

        let chat: ChatResponse = response.json().await.map_err(|e| LlmError::BadInput {
            reason: format!("ollama response parse failed: {e}"),
        })?;

        let payload: CardsPayload =
            serde_json::from_str(&chat.message.content).map_err(|e| LlmError::BadInput {
                reason: format!("ollama content parse failed: {e}"),
            })?;

        Ok(payload
            .cards
            .into_iter()
            .map(|c| ProposedCard {
                content: CardContent::Qa {
                    front: c.front,
                    back: c.back,
                },
                rationale: c.rationale,
            })
            .collect())
    }
}

fn response_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "cards": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "front": {"type": "string"},
                        "back": {"type": "string"},
                        "rationale": {"type": "string"}
                    },
                    "required": ["front", "back"]
                }
            }
        },
        "required": ["cards"]
    })
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    stream: bool,
    format: serde_json::Value,
    #[serde(skip_serializing_if = "ChatOptions::is_empty")]
    options: ChatOptions,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize, Default)]
struct ChatOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
}

impl ChatOptions {
    fn is_empty(&self) -> bool {
        self.temperature.is_none() && self.num_predict.is_none()
    }
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct CardsPayload {
    cards: Vec<CardJson>,
}

#[derive(Deserialize)]
struct CardJson {
    front: String,
    back: String,
    #[serde(default)]
    rationale: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn chunk() -> ChunkContext {
        ChunkContext {
            source_file: "n.md".to_string(),
            source_heading: Some("H".to_string()),
            tags: vec!["t".to_string()],
            text: "body".to_string(),
        }
    }

    fn provider_for(server: &MockServer) -> OllamaProvider {
        OllamaProvider::new(OllamaConfig {
            model: "test-model".to_string(),
            base_url: server.uri(),
            temperature: None,
            num_predict: None,
        })
    }

    async fn mount_chat_response(server: &MockServer, status: u16, body: serde_json::Value) {
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(server)
            .await;
    }

    fn envelope(content: &str) -> serde_json::Value {
        serde_json::json!({ "message": { "role": "assistant", "content": content } })
    }

    #[tokio::test]
    async fn parses_happy_path_response() {
        let server = MockServer::start().await;
        mount_chat_response(
            &server,
            200,
            envelope(
                r#"{"cards":[{"front":"Q1","back":"A1"},{"front":"Q2","back":"A2","rationale":"r"}]}"#,
            ),
        )
        .await;

        let cards = provider_for(&server)
            .propose_cards(&chunk())
            .await
            .expect("ok");
        assert_eq!(cards.len(), 2);
        match &cards[0].content {
            CardContent::Qa { front, back } => {
                assert_eq!(front, "Q1");
                assert_eq!(back, "A1");
            }
            _ => panic!("expected Qa, got {:?}", cards[0].content),
        }
        assert_eq!(cards[0].rationale, None);
        assert_eq!(cards[1].rationale.as_deref(), Some("r"));
    }

    #[tokio::test]
    async fn empty_cards_array_is_ok() {
        let server = MockServer::start().await;
        mount_chat_response(&server, 200, envelope(r#"{"cards":[]}"#)).await;

        let cards = provider_for(&server)
            .propose_cards(&chunk())
            .await
            .expect("ok");
        assert!(cards.is_empty());
    }

    #[tokio::test]
    async fn malformed_content_is_bad_input() {
        let server = MockServer::start().await;
        mount_chat_response(&server, 200, envelope("not json at all")).await;

        let err = provider_for(&server)
            .propose_cards(&chunk())
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::BadInput { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn non_2xx_is_config() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(500).set_body_string("model not loaded"))
            .mount(&server)
            .await;

        let err = provider_for(&server)
            .propose_cards(&chunk())
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Config { .. }), "got {err:?}");
    }
}
