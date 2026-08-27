use std::{collections::HashMap, process::Stdio, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};
use tokio_util::sync::CancellationToken;
use wisp_config::AiRuntimeConfig;
pub use wisp_config::{CompletionConfig, ProviderConfig, WispConfig as AiConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiCompletionRequest {
    pub request_id: u64,
    pub prefix: String,
    pub suffix: String,
    pub shell: String,
    pub cwd: String,
    #[serde(default)]
    pub recent_commands: Vec<String>,
    pub max_output_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiCompletion {
    pub suffix: String,
    pub confidence: Option<f32>,
    pub provider_id: String,
    pub model: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AiProviderError {
    #[error("provider request was cancelled")]
    Cancelled,
    #[error("provider timed out")]
    Timeout,
    #[error("provider configuration is invalid: {0}")]
    Configuration(String),
    #[error("provider request failed: {0}")]
    Request(String),
    #[error("provider returned an invalid response: {0}")]
    Response(String),
}

#[async_trait]
pub trait AiProvider: Send + Sync {
    fn id(&self) -> &str;

    async fn complete(
        &self,
        request: AiCompletionRequest,
        cancellation: CancellationToken,
    ) -> Result<AiCompletion, AiProviderError>;
}

#[derive(Default)]
pub struct ProviderRegistry {
    default_provider: Option<String>,
    providers: HashMap<String, Arc<dyn AiProvider>>,
}

impl ProviderRegistry {
    pub fn from_config(config: AiConfig) -> Result<Self, AiProviderError> {
        let runtime = config.ai.clone();
        let mut registry = Self {
            default_provider: config.default_provider,
            providers: HashMap::new(),
        };
        for (id, provider) in config.providers {
            let provider: Arc<dyn AiProvider> = match provider {
                ProviderConfig::OpenaiCompatible {
                    base_url,
                    model,
                    api_key_env,
                    timeout_ms,
                } => Arc::new(OpenAiCompatibleProvider::new(
                    id.clone(),
                    base_url,
                    model,
                    api_key_env,
                    timeout_ms,
                    runtime.clone(),
                )?),
                ProviderConfig::Process {
                    command,
                    timeout_ms,
                } => Arc::new(ProcessProvider::new(
                    id.clone(),
                    command,
                    timeout_ms,
                    runtime.clone(),
                )?),
            };
            registry.providers.insert(id, provider);
        }
        if let Some(default) = registry.default_provider.as_deref()
            && !registry.providers.contains_key(default)
        {
            return Err(AiProviderError::Configuration(format!(
                "default provider `{default}` does not exist"
            )));
        }
        Ok(registry)
    }

    pub fn is_enabled(&self) -> bool {
        self.default_provider.is_some()
    }

    pub async fn complete(
        &self,
        request: AiCompletionRequest,
        cancellation: CancellationToken,
    ) -> Result<Option<AiCompletion>, AiProviderError> {
        let Some(id) = self.default_provider.as_deref() else {
            return Ok(None);
        };
        let provider = self
            .providers
            .get(id)
            .ok_or_else(|| AiProviderError::Configuration(format!("missing provider `{id}`")))?;
        provider.complete(request, cancellation).await.map(Some)
    }
}

struct OpenAiCompatibleProvider {
    id: String,
    endpoint: String,
    model: String,
    api_key_env: Option<String>,
    timeout: Duration,
    client: reqwest::Client,
    runtime: AiRuntimeConfig,
}

impl OpenAiCompatibleProvider {
    fn new(
        id: String,
        base_url: String,
        model: String,
        api_key_env: Option<String>,
        timeout_ms: u64,
        runtime: AiRuntimeConfig,
    ) -> Result<Self, AiProviderError> {
        let base_url = base_url.trim_end_matches('/');
        let parsed = reqwest::Url::parse(base_url)
            .map_err(|error| AiProviderError::Configuration(error.to_string()))?;
        let endpoint = parsed
            .join(&format!(
                "{}/chat/completions",
                parsed.path().trim_start_matches('/')
            ))
            .map_err(|error| AiProviderError::Configuration(error.to_string()))?
            .to_string();
        Ok(Self {
            id,
            endpoint,
            model,
            api_key_env,
            timeout: Duration::from_millis(timeout_ms),
            client: reqwest::Client::new(),
            runtime,
        })
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
    max_tokens: u16,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[async_trait]
impl AiProvider for OpenAiCompatibleProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(
        &self,
        request: AiCompletionRequest,
        cancellation: CancellationToken,
    ) -> Result<AiCompletion, AiProviderError> {
        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: "Complete the shell command. Return only the suffix to insert, with no markdown or explanation. Never return a newline or terminal control characters.".into(),
                },
                ChatMessage {
                    role: "user",
                    content: format!(
                        "shell={}\ncwd={}\nprefix={}\nsuffix={}",
                        request.shell, request.cwd, request.prefix, request.suffix
                    ),
                },
            ],
            temperature: self.runtime.temperature,
            max_tokens: self.runtime.max_tokens,
        };
        let mut builder = self.client.post(&self.endpoint).json(&body);
        if let Some(environment) = self.api_key_env.as_deref() {
            let key = std::env::var(environment).map_err(|_| {
                AiProviderError::Configuration(format!(
                    "environment variable `{environment}` is not set"
                ))
            })?;
            builder = builder.bearer_auth(key);
        }

        let operation = async {
            let response = builder
                .send()
                .await
                .map_err(|error| AiProviderError::Request(error.to_string()))?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(AiProviderError::Request(format!(
                    "HTTP {status}: {}",
                    truncate(&body, self.runtime.provider_error_chars)
                )));
            }
            let response: ChatResponse = response
                .json()
                .await
                .map_err(|error| AiProviderError::Response(error.to_string()))?;
            let suffix = response
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| AiProviderError::Response("empty choices".into()))?
                .message
                .content;
            Ok(clean_suffix(&suffix, request.max_output_chars))
        };

        let suffix = tokio::select! {
            () = cancellation.cancelled() => return Err(AiProviderError::Cancelled),
            result = timeout(self.timeout, operation) => result.map_err(|_| AiProviderError::Timeout)??,
        };
        Ok(AiCompletion {
            suffix,
            confidence: None,
            provider_id: self.id.clone(),
            model: Some(self.model.clone()),
        })
    }
}

struct ProcessProvider {
    id: String,
    command: Vec<String>,
    timeout: Duration,
    runtime: AiRuntimeConfig,
}

impl ProcessProvider {
    fn new(
        id: String,
        command: Vec<String>,
        timeout_ms: u64,
        runtime: AiRuntimeConfig,
    ) -> Result<Self, AiProviderError> {
        if command.is_empty() {
            return Err(AiProviderError::Configuration(
                "process provider command cannot be empty".into(),
            ));
        }
        Ok(Self {
            id,
            command,
            timeout: Duration::from_millis(timeout_ms),
            runtime,
        })
    }
}

#[derive(Deserialize)]
struct ProcessResponse {
    suffix: String,
    confidence: Option<f32>,
    model: Option<String>,
}

#[async_trait]
impl AiProvider for ProcessProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete(
        &self,
        request: AiCompletionRequest,
        cancellation: CancellationToken,
    ) -> Result<AiCompletion, AiProviderError> {
        let mut child = Command::new(&self.command[0])
            .args(&self.command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| AiProviderError::Request(error.to_string()))?;
        let input = serde_json::to_vec(&request)
            .map_err(|error| AiProviderError::Request(error.to_string()))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| AiProviderError::Request("provider stdin is unavailable".into()))?;
        stdin
            .write_all(&input)
            .await
            .map_err(|error| AiProviderError::Request(error.to_string()))?;
        drop(stdin);

        let output = tokio::select! {
            () = cancellation.cancelled() => return Err(AiProviderError::Cancelled),
            result = timeout(self.timeout, child.wait_with_output()) => {
                result.map_err(|_| AiProviderError::Timeout)?
                    .map_err(|error| AiProviderError::Request(error.to_string()))?
            }
        };
        if !output.status.success() {
            return Err(AiProviderError::Request(format!(
                "process exited with {}: {}",
                output.status,
                truncate(
                    &String::from_utf8_lossy(&output.stderr),
                    self.runtime.provider_error_chars,
                )
            )));
        }
        let response: ProcessResponse = serde_json::from_slice(&output.stdout)
            .map_err(|error| AiProviderError::Response(error.to_string()))?;
        Ok(AiCompletion {
            suffix: clean_suffix(&response.suffix, request.max_output_chars),
            confidence: response.confidence,
            provider_id: self.id.clone(),
            model: response.model,
        })
    }
}

fn clean_suffix(value: &str, max_chars: usize) -> String {
    value
        .trim_matches(['\n', '\r'])
        .chars()
        .take_while(|ch| !ch.is_control())
        .take(max_chars)
        .collect()
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_default_provider() {
        let config = AiConfig {
            completion: CompletionConfig::default(),
            default_provider: Some("missing".into()),
            providers: HashMap::new(),
            ..Default::default()
        };
        assert!(ProviderRegistry::from_config(config).is_err());
    }

    #[test]
    fn sanitizes_provider_output() {
        assert_eq!(clean_suffix(" --release\nexplanation", 32), " --release");
        assert_eq!(clean_suffix("abcdef", 3), "abc");
    }

    #[test]
    fn example_configuration_is_valid() {
        let config: AiConfig =
            toml::from_str(include_str!("../../../config.example.toml")).unwrap();
        assert_eq!(config.completion.max_candidates, 0);
        assert!(ProviderRegistry::from_config(config).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_provider_uses_json_protocol() {
        let mut providers = HashMap::new();
        providers.insert(
            "custom".into(),
            ProviderConfig::Process {
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    r#"cat >/dev/null; printf '%s' '{"suffix":" --release","confidence":0.9,"model":"test"}'"#
                        .into(),
                ],
                timeout_ms: 500,
            },
        );
        let registry = ProviderRegistry::from_config(AiConfig {
            completion: CompletionConfig::default(),
            default_provider: Some("custom".into()),
            providers,
            ..Default::default()
        })
        .unwrap();
        let completion = registry
            .complete(
                AiCompletionRequest {
                    request_id: 1,
                    prefix: "cargo build".into(),
                    suffix: String::new(),
                    shell: "zsh".into(),
                    cwd: "/tmp".into(),
                    recent_commands: Vec::new(),
                    max_output_chars: 64,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completion.suffix, " --release");
        assert_eq!(completion.provider_id, "custom");
    }
}
