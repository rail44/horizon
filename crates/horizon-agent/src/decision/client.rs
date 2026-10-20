//! The decision-call seam and its HTTP implementation.

use async_trait::async_trait;

use super::config::DecisionConfig;
use super::wire::{DecisionRequest, DecisionResponse};

/// The longest stretch of an error body quoted back in an error.
const ERROR_BODY_LIMIT: usize = 2_000;

/// Asks a decision model one request's questions.
#[async_trait]
pub trait DecisionClient: Send + Sync {
    async fn decide(&self, request: DecisionRequest) -> anyhow::Result<DecisionResponse>;
}

/// [`DecisionClient`] over `POST {base_url}/systemone`.
pub struct HttpDecisionClient {
    config: DecisionConfig,
    http: reqwest::Client,
}

impl HttpDecisionClient {
    pub fn new(config: DecisionConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// The decision model id this connection's calls run on
    /// (`decision_model`).
    pub fn model(&self) -> &str {
        &self.config.model
    }
}

#[async_trait]
impl DecisionClient for HttpDecisionClient {
    async fn decide(&self, request: DecisionRequest) -> anyhow::Result<DecisionResponse> {
        let url = systemone_url(self.config.base_url.as_deref())?;
        // Read per call, so a key exported after startup is picked up.
        let api_key = std::env::var(&self.config.api_key_env).map_err(|_| {
            anyhow::anyhow!(
                "decision client unavailable ({} unset)",
                self.config.api_key_env
            )
        })?;
        let body = serde_json::to_vec(&request)?;
        let response = self
            .http
            .post(url)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(status_error(status.as_u16(), &text));
        }
        serde_json::from_str(&text)
            .map_err(|error| anyhow::anyhow!("decision response did not parse: {error}"))
    }
}

/// The System One route beside `/chat/completions` on the entry's base URL.
/// OpenAI's default URL serves no such route, so an entry without its own
/// `base_url` cannot be asked.
fn systemone_url(base_url: Option<&str>) -> anyhow::Result<String> {
    let base_url = base_url.ok_or_else(|| {
        anyhow::anyhow!(
            "decision client unavailable (the decision_model entry needs a base_url that \
             serves System One; OpenAI's default URL does not)"
        )
    })?;
    Ok(format!("{}/systemone", base_url.trim_end_matches('/')))
}

/// A non-2xx response as an error: the status plus the body's `error`
/// message, or the raw body when it carries none, bounded either way.
fn status_error(status: u16, body: &str) -> anyhow::Error {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("error")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| body.to_owned());
    let message: String = message.chars().take(ERROR_BODY_LIMIT).collect();
    anyhow::anyhow!("decision request failed with HTTP {status}: {message}")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::decision::{Answer, Question};

    #[test]
    fn the_route_joins_the_base_url_with_or_without_a_trailing_slash() {
        assert_eq!(
            systemone_url(Some("https://ollama.com/v1")).unwrap(),
            "https://ollama.com/v1/systemone"
        );
        assert_eq!(
            systemone_url(Some("https://ollama.com/v1/")).unwrap(),
            "https://ollama.com/v1/systemone"
        );
    }

    #[test]
    fn an_entry_without_a_base_url_cannot_be_asked() {
        let error = systemone_url(None).unwrap_err().to_string();
        assert!(error.contains("base_url"), "{error}");
        assert!(error.contains("System One"), "{error}");
    }

    #[test]
    fn a_non_success_status_carries_the_bodys_error_message() {
        let error = status_error(402, r#"{"error": "this model requires a subscription"}"#);
        assert_eq!(
            error.to_string(),
            "decision request failed with HTTP 402: this model requires a subscription"
        );
    }

    #[test]
    fn a_non_json_error_body_is_quoted_bounded() {
        let body = "x".repeat(5_000);
        let error = status_error(500, &body).to_string();
        assert!(error.starts_with("decision request failed with HTTP 500: "));
        assert_eq!(error.matches('x').count(), ERROR_BODY_LIMIT);
    }

    #[tokio::test]
    async fn an_unset_key_variable_fails_before_any_request() {
        let client = HttpDecisionClient::new(DecisionConfig {
            base_url: Some("http://127.0.0.1:9/v1".into()),
            api_key_env: "HORIZON_DECISION_TEST_UNSET_KEY".into(),
            model: "clef:cloud".into(),
        });
        let error = client.decide(noul_request()).await.unwrap_err().to_string();
        assert_eq!(
            error,
            "decision client unavailable (HORIZON_DECISION_TEST_UNSET_KEY unset)"
        );
    }

    fn noul_request() -> DecisionRequest {
        DecisionRequest {
            model: "clef:cloud".into(),
            state: serde_json::Value::String("the tests pass".into()),
            questions: BTreeMap::from([(
                "done".to_string(),
                Question::Noul {
                    instructions: "Is the work done?".into(),
                    criteria: None,
                },
            )]),
        }
    }

    /// Accepts one connection, answers it with `status` and `body`, and
    /// returns the raw request it read.
    async fn serve_once(
        listener: tokio::net::TcpListener,
        status: &'static str,
        body: &'static str,
    ) -> String {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let read = stream.read(&mut buf).await.unwrap();
            assert!(read > 0, "connection closed mid-request");
            request.extend_from_slice(&buf[..read]);
            let text = String::from_utf8_lossy(&request);
            let Some(head_end) = text.find("\r\n\r\n") else {
                continue;
            };
            let content_length = text[..head_end]
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if request.len() >= head_end + 4 + content_length {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
        String::from_utf8(request).unwrap()
    }

    #[tokio::test]
    async fn loopback_decide_posts_to_systemone_and_parses_the_answers() {
        let key_var = "HORIZON_DECISION_TEST_LOOPBACK_KEY";
        std::env::set_var(key_var, "secret-token");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_once(
            listener,
            "200 OK",
            r#"{"model":"clef","answers":{"done":{"type":"noul","noul":0.75}},"usage":{"input_tokens":12,"output_tokens":0}}"#,
        ));
        let client = HttpDecisionClient::new(DecisionConfig {
            base_url: Some(format!("http://{address}/v1/")),
            api_key_env: key_var.into(),
            model: "clef:cloud".into(),
        });
        assert_eq!(client.model(), "clef:cloud");
        let response = client.decide(noul_request()).await.unwrap();
        assert_eq!(response.answers["done"], Answer::Noul { noul: 0.75 });
        assert_eq!(response.usage.input_tokens, 12);

        let request = server.await.unwrap();
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(
            head.starts_with("POST /v1/systemone HTTP/1.1\r\n"),
            "{head}"
        );
        let head_lower = head.to_ascii_lowercase();
        assert!(
            head_lower.contains("authorization: bearer secret-token"),
            "{head}"
        );
        assert!(
            head_lower.contains("content-type: application/json"),
            "{head}"
        );
        let sent: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(sent, serde_json::to_value(noul_request()).unwrap());
    }

    #[tokio::test]
    async fn loopback_decide_maps_an_error_status_to_its_message() {
        let key_var = "HORIZON_DECISION_TEST_LOOPBACK_ERROR_KEY";
        std::env::set_var(key_var, "secret-token");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_once(
            listener,
            "413 Payload Too Large",
            r#"{"error":"input exceeds 65536 tokens"}"#,
        ));
        let client = HttpDecisionClient::new(DecisionConfig {
            base_url: Some(format!("http://{address}/v1")),
            api_key_env: key_var.into(),
            model: "clef:cloud".into(),
        });
        let error = client.decide(noul_request()).await.unwrap_err().to_string();
        assert_eq!(
            error,
            "decision request failed with HTTP 413: input exceeds 65536 tokens"
        );
        server.await.unwrap();
    }
}
