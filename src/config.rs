use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub store: StoreConfig,
    pub llm: LlmConfig,
}

#[derive(Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub api_token: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoreConfig {
    pub data_dir: PathBuf,
}

#[derive(Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub temperature: Option<f32>,
    pub num_predict: Option<u32>,
    pub api_key: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080"
                .parse()
                .expect("valid default bind address"),
            api_token: None,
        }
    }
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./studybuddy-data"),
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "ollama".to_string(),
            model: "gpt-oss:120b-cloud".to_string(),
            base_url: "http://127.0.0.1:11434".to_string(),
            temperature: Some(0.2_f32),
            num_predict: None,
            api_key: None,
        }
    }
}

impl Config {
    pub fn from_toml_str(s: &str) -> anyhow::Result<Self> {
        toml::from_str(s).context("failed to parse config")
    }

    pub fn load() -> anyhow::Result<Self> {
        if let Ok(path) = std::env::var("STUDYBUDDY_CONFIG") {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read config from STUDYBUDDY_CONFIG={path}"))?;
            return Self::from_toml_str(&content);
        }

        let path = std::path::Path::new("studybuddy.toml");
        if path.exists() {
            let content =
                std::fs::read_to_string(path).context("failed to read studybuddy.toml")?;
            return Self::from_toml_str(&content);
        }

        Ok(Self::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty() {
        let cfg = Config::from_toml_str("").unwrap();
        assert_eq!(cfg.server.bind.to_string(), "127.0.0.1:8080");
        assert_eq!(cfg.store.data_dir, PathBuf::from("./studybuddy-data"));
        assert_eq!(cfg.llm.provider, "ollama");
        assert_eq!(cfg.llm.model, "gpt-oss:120b-cloud");
        assert_eq!(cfg.llm.base_url, "http://127.0.0.1:11434");
        assert_eq!(cfg.llm.temperature, Some(0.2_f32));
        assert_eq!(cfg.llm.num_predict, None);
        assert_eq!(cfg.llm.api_key, None);
    }

    #[test]
    fn partial_llm_section_preserves_other_defaults() {
        let cfg = Config::from_toml_str(
            r#"
[llm]
model = "qwen3:8b"
"#,
        )
        .unwrap();
        assert_eq!(cfg.llm.model, "qwen3:8b");
        assert_eq!(cfg.llm.provider, "ollama");
        assert_eq!(cfg.server.bind.to_string(), "127.0.0.1:8080");
        assert_eq!(cfg.store.data_dir, PathBuf::from("./studybuddy-data"));
    }

    #[test]
    fn full_config_parses() {
        let cfg = Config::from_toml_str(
            r#"
[server]
bind = "0.0.0.0:9090"

[store]
data_dir = "/var/lib/studybuddy"

[llm]
provider    = "ollama"
model       = "qwen3:8b"
base_url    = "http://192.168.1.100:11434"
temperature = 0.5
num_predict = 1024
"#,
        )
        .unwrap();
        assert_eq!(cfg.server.bind.to_string(), "0.0.0.0:9090");
        assert_eq!(cfg.store.data_dir, PathBuf::from("/var/lib/studybuddy"));
        assert_eq!(cfg.llm.model, "qwen3:8b");
        assert_eq!(cfg.llm.base_url, "http://192.168.1.100:11434");
        assert_eq!(cfg.llm.temperature, Some(0.5_f32));
        assert_eq!(cfg.llm.num_predict, Some(1024));
    }

    #[test]
    fn typo_in_llm_field_is_rejected() {
        let result = Config::from_toml_str(
            r#"
[llm]
temprature = 0.5
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn unknown_top_level_section_is_rejected() {
        let result = Config::from_toml_str(
            r#"
[logging]
level = "debug"
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn api_token_defaults_to_none_and_can_be_set() {
        let cfg = Config::from_toml_str("").unwrap();
        assert_eq!(cfg.server.api_token, None);

        let cfg = Config::from_toml_str(
            r#"
[server]
api_token = "mysecret"
"#,
        )
        .unwrap();
        assert_eq!(cfg.server.api_token.as_deref(), Some("mysecret"));
    }
}
