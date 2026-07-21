use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::HeaderValue;
use reqwest::{redirect, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::EXA_API_KEY_VAR;

const EXA_SEARCH_ENDPOINT: &str = "https://api.exa.ai/search";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_CONTENT_CHARACTERS: usize = 20_000;
const MAX_NORMALIZED_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_TITLE_CHARACTERS: usize = 500;
const MAX_RESULT_URL_CHARACTERS: usize = 2_048;
const MAX_PUBLISHED_DATE_CHARACTERS: usize = 128;
const MAX_AUTHOR_CHARACTERS: usize = 500;
const DEFAULT_RESULTS: usize = 5;
const MAX_RESULTS: usize = 10;
const DEFAULT_MAX_CHARACTERS: usize = 2_000;
const MAX_CHARACTERS: usize = 4_000;
const MAX_QUERY_CHARS: usize = 2_048;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    query: String,
    #[serde(default = "default_results")]
    num_results: usize,
    #[serde(default = "default_max_characters")]
    max_characters: usize,
}

fn default_results() -> usize {
    DEFAULT_RESULTS
}

fn default_max_characters() -> usize {
    DEFAULT_MAX_CHARACTERS
}

#[derive(Clone, Debug)]
struct SearchRequest {
    query: String,
    num_results: usize,
    max_characters: usize,
}

#[derive(Clone, Debug, Serialize)]
struct SearchResult {
    title: String,
    url: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<String>,
}

#[derive(Clone, Debug)]
struct SearchResponse {
    results: Vec<SearchResult>,
}

#[async_trait]
trait SearchAdapter: Send + Sync {
    async fn search(&self, request: SearchRequest) -> Result<SearchResponse, String>;
}

struct ExaAdapter {
    client: Client,
    endpoint: Url,
    api_key: String,
}

impl ExaAdapter {
    fn production(api_key: String) -> Result<Self, String> {
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| format!("failed to build the Exa client: {error}"))?;
        let endpoint = Url::parse(EXA_SEARCH_ENDPOINT)
            .map_err(|error| format!("invalid built-in Exa endpoint: {error}"))?;
        Ok(Self {
            client,
            endpoint,
            api_key,
        })
    }
}

#[async_trait]
impl SearchAdapter for ExaAdapter {
    async fn search(&self, request: SearchRequest) -> Result<SearchResponse, String> {
        let body = ExaSearchRequest {
            query: &request.query,
            num_results: request.num_results,
            contents: ExaContents {
                highlights: ExaHighlights {
                    query: &request.query,
                    max_characters: request.max_characters,
                },
            },
        };
        let mut api_key = HeaderValue::from_str(&self.api_key)
            .map_err(|_| "EXA_API_KEY is not a valid HTTP header value".to_string())?;
        api_key.set_sensitive(true);
        let mut response = self
            .client
            .post(self.endpoint.clone())
            .header("x-api-key", api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("Exa search request failed: {error}"))?;
        let status = response.status();
        let bytes = read_capped(&mut response, MAX_RESPONSE_BYTES).await?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes);
            let body = redact_secret(body.trim(), &self.api_key);
            return Err(format!(
                "Exa search returned HTTP {status}: {}",
                truncate_chars(&body, 2_000)
            ));
        }
        let response: ExaSearchResponse = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Exa search returned invalid JSON: {error}"))?;
        let mut remaining = MAX_TOTAL_CONTENT_CHARACTERS;
        let mut results = Vec::new();
        for result in response.results.into_iter().take(request.num_results) {
            let Some(url) = normalize_result_url(redact_secret(&result.url, &self.api_key)) else {
                continue;
            };
            let content = if result.highlights.is_empty() {
                result.text.unwrap_or_default()
            } else {
                result.highlights.join("\n\n")
            };
            let content = redact_secret(&content, &self.api_key);
            let budget = remaining.min(request.max_characters);
            let content = truncate_chars(&content, budget);
            remaining = remaining.saturating_sub(content.chars().count());
            results.push(SearchResult {
                title: truncate_chars(
                    &redact_secret(&result.title.unwrap_or_default(), &self.api_key),
                    MAX_TITLE_CHARACTERS,
                ),
                url,
                content,
                published_date: result.published_date.map(|value| {
                    truncate_chars(
                        &redact_secret(&value, &self.api_key),
                        MAX_PUBLISHED_DATE_CHARACTERS,
                    )
                }),
                author: result.author.map(|value| {
                    truncate_chars(&redact_secret(&value, &self.api_key), MAX_AUTHOR_CHARACTERS)
                }),
            });
            if remaining == 0 {
                break;
            }
        }
        Ok(SearchResponse { results })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExaSearchRequest<'a> {
    query: &'a str,
    num_results: usize,
    contents: ExaContents<'a>,
}

#[derive(Serialize)]
struct ExaContents<'a> {
    highlights: ExaHighlights<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExaHighlights<'a> {
    query: &'a str,
    max_characters: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExaSearchResponse {
    #[serde(default)]
    results: Vec<ExaResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExaResult {
    #[serde(default)]
    title: Option<String>,
    url: String,
    #[serde(default)]
    published_date: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    highlights: Vec<String>,
}

pub(super) async fn execute(input: Value) -> Value {
    let input: SearchInput = match serde_json::from_value(input) {
        Ok(input) => input,
        Err(error) => return error_output(format!("invalid web_search input: {error}")),
    };
    if let Err(message) = validate(&input) {
        return error_output(message);
    }
    let api_key = match std::env::var(EXA_API_KEY_VAR) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            return error_output(format!(
                "web_search is not configured; set {EXA_API_KEY_VAR} in Horizon's environment"
            ))
        }
    };
    let adapter = match ExaAdapter::production(api_key) {
        Ok(adapter) => adapter,
        Err(message) => return error_output(message),
    };
    execute_with_adapter(
        &adapter,
        SearchRequest {
            query: input.query.clone(),
            num_results: input.num_results,
            max_characters: input.max_characters,
        },
    )
    .await
}

async fn execute_with_adapter(adapter: &dyn SearchAdapter, request: SearchRequest) -> Value {
    let query = request.query.clone();
    match adapter.search(request).await {
        Ok(response) => {
            let output = json!({
                "is_error": false,
                "query": query,
                "results": response.results,
            });
            if serde_json::to_vec(&output)
                .is_ok_and(|bytes| bytes.len() <= MAX_NORMALIZED_OUTPUT_BYTES)
            {
                output
            } else {
                error_output(format!(
                    "normalized web_search output exceeded the {MAX_NORMALIZED_OUTPUT_BYTES} byte limit"
                ))
            }
        }
        Err(message) => error_output(message),
    }
}

fn validate(input: &SearchInput) -> Result<(), String> {
    let query_chars = input.query.chars().count();
    if input.query.trim().is_empty() {
        return Err("web_search query must not be empty".to_string());
    }
    if query_chars > MAX_QUERY_CHARS {
        return Err(format!(
            "web_search query exceeds the {MAX_QUERY_CHARS} character limit"
        ));
    }
    if !(1..=MAX_RESULTS).contains(&input.num_results) {
        return Err(format!(
            "web_search num_results must be between 1 and {MAX_RESULTS}"
        ));
    }
    if !(1..=MAX_CHARACTERS).contains(&input.max_characters) {
        return Err(format!(
            "web_search max_characters must be between 1 and {MAX_CHARACTERS}"
        ));
    }
    Ok(())
}

async fn read_capped(response: &mut reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Exa response body failed: {error}"))?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(format!("Exa response exceeded the {limit} byte limit"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.char_indices()
        .nth(max)
        .map_or_else(|| text.to_string(), |(end, _)| text[..end].to_string())
}

fn normalize_result_url(value: String) -> Option<String> {
    if value.chars().count() > MAX_RESULT_URL_CHARACTERS {
        return None;
    }
    let url = Url::parse(&value).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(value)
}

fn redact_secret(value: &str, secret: &str) -> String {
    if secret.is_empty() {
        value.to_string()
    } else {
        value.replace(secret, "[REDACTED]")
    }
}

fn error_output(message: String) -> Value {
    json!({ "is_error": true, "message": message })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct StubAdapter {
        seen: Mutex<Vec<SearchRequest>>,
        response: Mutex<Option<Result<SearchResponse, String>>>,
    }

    #[async_trait]
    impl SearchAdapter for StubAdapter {
        async fn search(&self, request: SearchRequest) -> Result<SearchResponse, String> {
            self.seen.lock().unwrap().push(request);
            self.response.lock().unwrap().take().unwrap()
        }
    }

    #[tokio::test]
    async fn adapter_output_is_normalized_without_exposing_vendor_shape() {
        let adapter = StubAdapter {
            seen: Mutex::new(Vec::new()),
            response: Mutex::new(Some(Ok(SearchResponse {
                results: vec![SearchResult {
                    title: "Title".to_string(),
                    url: "https://example.com".to_string(),
                    content: "Excerpt".to_string(),
                    published_date: None,
                    author: Some("Author".to_string()),
                }],
            }))),
        };
        let output = execute_with_adapter(
            &adapter,
            SearchRequest {
                query: "rust".to_string(),
                num_results: 3,
                max_characters: 500,
            },
        )
        .await;
        assert!(output.get("provider").is_none());
        assert!(output.get("request_id").is_none());
        assert_eq!(output["results"][0]["content"], "Excerpt");
        let seen = adapter.seen.lock().unwrap();
        assert_eq!(seen[0].num_results, 3);
        assert_eq!(seen[0].max_characters, 500);
    }

    #[test]
    fn validation_caps_query_results_and_content_budget() {
        let valid = SearchInput {
            query: "rust".to_string(),
            num_results: MAX_RESULTS,
            max_characters: MAX_CHARACTERS,
        };
        assert!(validate(&valid).is_ok());
        assert!(validate(&SearchInput {
            query: "".to_string(),
            ..valid.clone()
        })
        .is_err());
        assert!(validate(&SearchInput {
            num_results: MAX_RESULTS + 1,
            ..valid.clone()
        })
        .is_err());
        assert!(validate(&SearchInput {
            max_characters: MAX_CHARACTERS + 1,
            ..valid
        })
        .is_err());
    }

    #[test]
    fn result_urls_remain_whole_and_secrets_are_redacted() {
        assert_eq!(
            normalize_result_url("https://example.com/path".to_string()).as_deref(),
            Some("https://example.com/path")
        );
        assert!(normalize_result_url("javascript:alert(1)".to_string()).is_none());
        assert!(normalize_result_url(format!(
            "https://example.com/{}",
            "x".repeat(MAX_RESULT_URL_CHARACTERS)
        ))
        .is_none());
        assert_eq!(
            redact_secret("server echoed test-secret", "test-secret"),
            "server echoed [REDACTED]"
        );
    }

    #[tokio::test]
    async fn exa_adapter_uses_the_fixed_post_contract_and_normalizes_results() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut socket).await;
            let header_end = find_bytes(&request, b"\r\n\r\n").unwrap() + 4;
            let head = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            assert!(head.starts_with("post /search http/1.1\r\n"));
            assert!(head.contains("\r\nx-api-key: test-secret\r\n"));
            let body: Value = serde_json::from_slice(&request[header_end..]).unwrap();
            assert_eq!(body["query"], "rust agents");
            assert_eq!(body["numResults"], 3);
            assert_eq!(body["contents"]["highlights"]["query"], "rust agents");
            assert_eq!(body["contents"]["highlights"]["maxCharacters"], 500);

            let response_body = serde_json::to_vec(&json!({
                "requestId": "req-local",
                "results": [{
                    "title": "Result test-secret",
                    "url": "https://example.com/result",
                    "highlights": ["first test-secret", "second"],
                    "publishedDate": "2026-07-21",
                    "author": "test-secret author",
                    "ignoredVendorField": true
                }]
            }))
            .unwrap();
            let response_head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_body.len()
            );
            socket.write_all(response_head.as_bytes()).await.unwrap();
            socket.write_all(&response_body).await.unwrap();
        });

        let adapter = ExaAdapter {
            client: Client::builder()
                .no_proxy()
                .redirect(redirect::Policy::none())
                .build()
                .unwrap(),
            endpoint: Url::parse(&format!("http://{addr}/search")).unwrap(),
            api_key: "test-secret".to_string(),
        };
        let response = adapter
            .search(SearchRequest {
                query: "rust agents".to_string(),
                num_results: 3,
                max_characters: 500,
            })
            .await
            .unwrap();
        server.await.unwrap();

        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].content, "first [REDACTED]\n\nsecond");
        assert_eq!(response.results[0].title, "Result [REDACTED]");
        assert_eq!(
            response.results[0].author.as_deref(),
            Some("[REDACTED] author")
        );
        assert_eq!(
            response.results[0].published_date.as_deref(),
            Some("2026-07-21")
        );
    }

    async fn read_http_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            assert!(read > 0, "client closed before sending the full request");
            request.extend_from_slice(&buffer[..read]);
            let Some(header_start) = find_bytes(&request, b"\r\n\r\n") else {
                continue;
            };
            let header_end = header_start + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            if request.len() >= header_end + content_length {
                request.truncate(header_end + content_length);
                return request;
            }
        }
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}
