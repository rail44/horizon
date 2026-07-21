use std::sync::Arc;
use std::time::Duration;

use dom_smoothie::{Config, Readability, TextMode};
use horizon_sandbox_proxy::Allowlist;
use reqwest::header::{
    ACCEPT, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, LOCATION,
};
use reqwest::{redirect, Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::{json, Value};

use super::ssrf::{remote_addr_is_safe, validate_url, SafeResolver};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_MAX_CHARACTERS: usize = 20_000;
const MAX_CHARACTERS: usize = 50_000;
const MAX_DOM_ELEMENTS: usize = 20_000;
const MAX_TITLE_CHARACTERS: usize = 1_000;
const MAX_BYLINE_CHARACTERS: usize = 1_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchInput {
    url: String,
    #[serde(default = "default_max_characters")]
    max_characters: usize,
}

fn default_max_characters() -> usize {
    DEFAULT_MAX_CHARACTERS
}

pub(super) enum FetchOutcome {
    Finished(Value),
    DomainGrantRequired(Vec<String>),
}

pub(super) fn domain_from_input(input: &Value) -> Result<String, String> {
    let input: FetchInput = serde_json::from_value(input.clone())
        .map_err(|error| format!("invalid web_fetch input: {error}"))?;
    validate_input(&input)?;
    let url = Url::parse(&input.url).map_err(|error| format!("invalid web_fetch URL: {error}"))?;
    validate_url(&url)
}

pub(super) async fn execute(input: Value, domains: Arc<Allowlist>) -> FetchOutcome {
    let input: FetchInput = match serde_json::from_value(input) {
        Ok(input) => input,
        Err(error) => {
            return FetchOutcome::Finished(error_output(format!(
                "invalid web_fetch input: {error}"
            )))
        }
    };
    if let Err(message) = validate_input(&input) {
        return FetchOutcome::Finished(error_output(message));
    }
    let initial_url = match Url::parse(&input.url) {
        Ok(url) => url,
        Err(error) => {
            return FetchOutcome::Finished(error_output(format!("invalid web_fetch URL: {error}")))
        }
    };

    let fetched = tokio::time::timeout(
        REQUEST_TIMEOUT,
        fetch(initial_url, input.max_characters, domains),
    )
    .await;
    match fetched {
        Err(_) => FetchOutcome::Finished(error_output(format!(
            "web_fetch exceeded the {} second total timeout",
            REQUEST_TIMEOUT.as_secs()
        ))),
        Ok(result) => match result {
            Ok(FetchOutcome::Finished(output)) => FetchOutcome::Finished(output),
            Ok(FetchOutcome::DomainGrantRequired(domains)) => {
                FetchOutcome::DomainGrantRequired(domains)
            }
            Err(message) => FetchOutcome::Finished(error_output(message)),
        },
    }
}

fn validate_input(input: &FetchInput) -> Result<(), String> {
    if !(1..=MAX_CHARACTERS).contains(&input.max_characters) {
        return Err(format!(
            "web_fetch max_characters must be between 1 and {MAX_CHARACTERS}"
        ));
    }
    Ok(())
}

async fn fetch(
    initial_url: Url,
    max_characters: usize,
    domains: Arc<Allowlist>,
) -> Result<FetchOutcome, String> {
    let client = Client::builder()
        .dns_resolver(SafeResolver)
        .no_proxy()
        .redirect(redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| format!("failed to build the web_fetch client: {error}"))?;

    let mut url = initial_url;
    for redirect_count in 0..=MAX_REDIRECTS {
        let domain = validate_url(&url)?;
        if !domains.is_allowed(&domain) {
            return Ok(FetchOutcome::DomainGrantRequired(vec![domain]));
        }

        let mut response = client
            .get(url.clone())
            .header(
                ACCEPT,
                "text/html,application/xhtml+xml,text/plain,application/json,application/xml;q=0.9,*/*;q=0.1",
            )
            .header(ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|error| format!("web_fetch request failed: {error}"))?;

        if !remote_addr_is_safe(response.remote_addr()) {
            return Err("web_fetch connected to a non-public address".to_string());
        }

        if is_followed_redirect(response.status()) {
            if redirect_count == MAX_REDIRECTS {
                return Err(format!("web_fetch exceeded {MAX_REDIRECTS} redirects"));
            }
            let location = response
                .headers()
                .get(LOCATION)
                .ok_or_else(|| "web_fetch redirect had no Location header".to_string())?
                .to_str()
                .map_err(|_| "web_fetch redirect Location was not valid text".to_string())?;
            url = url
                .join(location)
                .map_err(|error| format!("invalid web_fetch redirect URL: {error}"))?;
            continue;
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = read_capped(&mut response, 8 * 1024)
                .await
                .unwrap_or_default();
            let message = String::from_utf8_lossy(&body);
            return Err(format!(
                "web_fetch returned HTTP {status}: {}",
                truncate_chars(message.trim(), 2_000).0
            ));
        }

        if response
            .headers()
            .get(CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|encoding| !encoding.eq_ignore_ascii_case("identity"))
        {
            return Err("web_fetch refuses encoded response bodies".to_string());
        }

        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > MAX_BODY_BYTES)
        {
            return Err(format!(
                "web_fetch response exceeded the {} byte limit",
                MAX_BODY_BYTES
            ));
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !supported_content_type(&content_type) {
            return Err(format!(
                "web_fetch refuses binary or unsupported content type `{content_type}`"
            ));
        }

        let bytes = read_capped(&mut response, MAX_BODY_BYTES).await?;
        let text = String::from_utf8(bytes)
            .map_err(|_| "web_fetch response was not valid UTF-8 text".to_string())?;
        let final_url = url.to_string();

        return if is_html(&content_type) {
            let extracted = extract_html(text, final_url.clone()).await?;
            let (content, truncated) = truncate_chars(&extracted.content, max_characters);
            let title = truncate_chars(&extracted.title, MAX_TITLE_CHARACTERS).0;
            let byline = extracted
                .byline
                .map(|byline| truncate_chars(&byline, MAX_BYLINE_CHARACTERS).0);
            Ok(FetchOutcome::Finished(json!({
                "is_error": false,
                "url": final_url,
                "title": title,
                "byline": byline,
                "content_type": content_type,
                "content": content,
                "truncated": truncated,
            })))
        } else {
            let (content, truncated) = truncate_chars(&text, max_characters);
            Ok(FetchOutcome::Finished(json!({
                "is_error": false,
                "url": final_url,
                "content_type": content_type,
                "content": content,
                "truncated": truncated,
            })))
        };
    }

    Err("web_fetch redirect loop ended unexpectedly".to_string())
}

struct ExtractedArticle {
    title: String,
    byline: Option<String>,
    content: String,
}

async fn extract_html(html: String, url: String) -> Result<ExtractedArticle, String> {
    tokio::task::spawn_blocking(move || {
        let config = Config {
            max_elements_to_parse: MAX_DOM_ELEMENTS,
            text_mode: TextMode::Markdown,
            ..Config::default()
        };
        let mut readability = Readability::new(html, Some(&url), Some(config))
            .map_err(|error| format!("web_fetch could not parse HTML: {error}"))?;
        let article = readability
            .parse()
            .map_err(|error| format!("web_fetch could not extract readable content: {error}"))?;
        Ok(ExtractedArticle {
            title: article.title,
            byline: article.byline,
            content: article.text_content.to_string(),
        })
    })
    .await
    .map_err(|error| format!("web_fetch HTML extractor stopped unexpectedly: {error}"))?
}

async fn read_capped(response: &mut reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("web_fetch response body failed: {error}"))?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(format!(
                "web_fetch response exceeded the {limit} byte limit"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn is_followed_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

fn is_html(content_type: &str) -> bool {
    matches!(content_type, "text/html" | "application/xhtml+xml")
}

fn supported_content_type(content_type: &str) -> bool {
    is_html(content_type)
        || content_type.starts_with("text/")
        || matches!(content_type, "application/json" | "application/xml")
        || content_type.ends_with("+json")
        || content_type.ends_with("+xml")
}

fn truncate_chars(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((end, _)) => (text[..end].to_string(), true),
        None => (text.to_string(), false),
    }
}

fn error_output(message: String) -> Value {
    json!({ "is_error": true, "message": message })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_domain_is_canonical_and_validation_is_fail_closed() {
        assert_eq!(
            domain_from_input(&json!({ "url": "https://Example.COM./docs" })).unwrap(),
            "example.com"
        );
        assert!(domain_from_input(&json!({ "url": "file:///etc/passwd" })).is_err());
        assert!(domain_from_input(&json!({
            "url": "https://example.com",
            "max_characters": MAX_CHARACTERS + 1
        }))
        .is_err());
        assert!(domain_from_input(&json!({
            "url": "https://example.com",
            "surprise": true
        }))
        .is_err());
    }

    #[test]
    fn truncation_respects_utf8_boundaries() {
        assert_eq!(truncate_chars("aé日", 2), ("aé".to_string(), true));
        assert_eq!(truncate_chars("aé日", 3), ("aé日".to_string(), false));
    }

    #[tokio::test]
    async fn html_is_extracted_as_markdown_with_a_dom_limit() {
        let extracted = extract_html(
            "<html><head><title>Example</title></head><body><article><h1>Hello</h1><p>Readable body text long enough for extraction.</p></article></body></html>".to_string(),
            "https://example.com/".to_string(),
        )
        .await
        .unwrap();
        assert_eq!(extracted.title, "Example");
        assert!(extracted.content.contains("Hello"));
        assert!(extracted.content.contains("Readable body"));
    }

    #[tokio::test]
    async fn an_unapproved_initial_domain_stops_before_network_io() {
        let domains = Arc::new(Allowlist::default());
        let outcome = execute(
            json!({ "url": "https://example.invalid/never-resolve" }),
            domains,
        )
        .await;
        match outcome {
            FetchOutcome::DomainGrantRequired(domains) => {
                assert_eq!(domains, vec!["example.invalid"]);
            }
            FetchOutcome::Finished(output) => {
                panic!("expected a domain grant before any request, got {output}")
            }
        }
    }
}
