use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::lock::Mutex as AsyncMutex;
use reqwest::header::{ACCEPT, CONTENT_TYPE, RETRY_AFTER};
use reqwest::{redirect, Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const SOURCEGRAPH_STREAM_ENDPOINT: &str = "https://sourcegraph.com/.api/search/stream";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_COOLDOWN: Duration = Duration::from_secs(5 * 60);
const MAX_RAW_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_NORMALIZED_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_QUERY_CHARACTERS: usize = 2_048;
const DEFAULT_RESULTS: usize = 10;
const MAX_RESULTS: usize = 20;
const DEFAULT_CONTEXT_LINES: usize = 2;
const MAX_CONTEXT_LINES: usize = 5;
const MAX_REPOSITORY_CHARACTERS: usize = 1_024;
const MAX_PATH_CHARACTERS: usize = 2_048;
const MAX_COMMIT_CHARACTERS: usize = 128;
const MAX_LANGUAGE_CHARACTERS: usize = 128;
const MAX_SNIPPET_CHARACTERS: usize = 4_000;
const MAX_ERROR_CHARACTERS: usize = 2_000;
const MAX_WARNINGS: usize = 5;
const MAX_WARNING_CHARACTERS: usize = 500;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeSearchInput {
    query: String,
    #[serde(default = "default_results")]
    num_results: usize,
    #[serde(default = "default_context_lines")]
    context_lines: usize,
}

fn default_results() -> usize {
    DEFAULT_RESULTS
}

fn default_context_lines() -> usize {
    DEFAULT_CONTEXT_LINES
}

#[derive(Clone, Debug)]
struct CodeSearchRequest {
    query: String,
    backend_query: String,
    num_results: usize,
    context_lines: usize,
}

#[derive(Clone, Debug, Serialize)]
struct CodeSearchResult {
    repository: String,
    path: String,
    commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    line_number: usize,
    snippet: String,
}

#[derive(Clone, Debug)]
struct CodeSearchResponse {
    results: Vec<CodeSearchResult>,
    match_count: Option<usize>,
    truncated: bool,
    warnings: Vec<String>,
}

#[async_trait]
trait CodeSearchAdapter: Send + Sync {
    async fn search(&self, request: CodeSearchRequest) -> Result<CodeSearchResponse, String>;
}

struct SourcegraphAdapter {
    client: Client,
    endpoint: Url,
}

impl SourcegraphAdapter {
    fn production() -> Result<Self, String> {
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| format!("failed to build the public-code search client: {error}"))?;
        let endpoint = Url::parse(SOURCEGRAPH_STREAM_ENDPOINT)
            .map_err(|error| format!("invalid built-in public-code search endpoint: {error}"))?;
        Ok(Self { client, endpoint })
    }
}

#[async_trait]
impl CodeSearchAdapter for SourcegraphAdapter {
    async fn search(&self, request: CodeSearchRequest) -> Result<CodeSearchResponse, String> {
        let mut endpoint = self.endpoint.clone();
        endpoint
            .query_pairs_mut()
            .append_pair("q", &request.backend_query)
            .append_pair("v", "V3")
            .append_pair("cm", "true")
            .append_pair("display", &request.num_results.to_string())
            .append_pair("max-line-len", "500")
            .append_pair("cl", &request.context_lines.to_string());

        let mut response = self
            .client
            .get(endpoint)
            .header(ACCEPT, "text/event-stream")
            .header("user-agent", "horizon-agent public-code-search")
            .send()
            .await
            .map_err(|error| format!("public-code search request failed: {error}"))?;
        let status = response.status();
        if matches!(
            status,
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
        ) {
            let duration = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_COOLDOWN)
                .min(MAX_COOLDOWN);
            note_cooldown(duration);
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = read_capped(&mut response, MAX_RAW_RESPONSE_BYTES).await?;
        if !status.is_success() {
            return Err(format!(
                "public-code search returned HTTP {status}: {}",
                truncate_chars(&String::from_utf8_lossy(&body), MAX_ERROR_CHARACTERS)
            ));
        }
        if !content_type
            .as_deref()
            .is_some_and(|value| value.starts_with("text/event-stream"))
        {
            return Err("public-code search returned a non-event-stream response".to_string());
        }
        parse_stream(&body, request.num_results)
    }
}

pub(super) async fn execute(input: Value) -> Value {
    let input: CodeSearchInput = match serde_json::from_value(input) {
        Ok(input) => input,
        Err(error) => return error_output(format!("invalid public_code_search input: {error}")),
    };
    let request = match prepare_request(input) {
        Ok(request) => request,
        Err(message) => return error_output(message),
    };

    let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
        // Sourcegraph.com publishes no anonymous quota or SLA. One in-flight
        // request plus a minimum start interval keeps this best-effort adapter
        // from becoming a bulk crawler. Contention is explicit rather than
        // silently consuming another call's total timeout in a queue.
        let mut gate = request_gate().try_lock().ok_or_else(|| {
            "public-code search is busy; wait before issuing another search".to_string()
        })?;
        if let Some(remaining) = cooldown_remaining() {
            return Err(format!(
                "public-code search is cooling down after a service refusal; retry in about {} seconds",
                remaining.as_secs().saturating_add(1)
            ));
        }
        if let Some(next_start) = gate.next_start {
            let now = Instant::now();
            if next_start > now {
                tokio::time::sleep(next_start.duration_since(now)).await;
            }
        }
        gate.next_start = Some(Instant::now() + MIN_REQUEST_INTERVAL);
        let adapter = SourcegraphAdapter::production()?;
        execute_with_adapter(&adapter, request).await
    })
    .await;
    match result {
        Ok(Ok(output)) => output,
        Ok(Err(message)) => error_output(message),
        Err(_) => error_output(format!(
            "public-code search exceeded the {} second total timeout",
            REQUEST_TIMEOUT.as_secs()
        )),
    }
}

async fn execute_with_adapter(
    adapter: &dyn CodeSearchAdapter,
    request: CodeSearchRequest,
) -> Result<Value, String> {
    let query = request.query.clone();
    let response = adapter.search(request).await?;
    let output = json!({
        "is_error": false,
        "query": query,
        "match_count": response.match_count,
        "truncated": response.truncated,
        "warnings": response.warnings,
        "results": response.results,
    });
    if serde_json::to_vec(&output).is_ok_and(|bytes| bytes.len() <= MAX_NORMALIZED_OUTPUT_BYTES) {
        Ok(output)
    } else {
        Err(format!(
            "normalized public_code_search output exceeded the {MAX_NORMALIZED_OUTPUT_BYTES} byte limit"
        ))
    }
}

fn prepare_request(input: CodeSearchInput) -> Result<CodeSearchRequest, String> {
    validate(&input)?;
    let query = input.query.trim().to_string();
    let backend_query = format!(
        "context:global repo:^github\\.com/ visibility:public type:file count:{} timeout:10s fork:no archived:no ({query})",
        input.num_results
    );
    Ok(CodeSearchRequest {
        query,
        backend_query,
        num_results: input.num_results,
        context_lines: input.context_lines,
    })
}

fn validate(input: &CodeSearchInput) -> Result<(), String> {
    if input.query.trim().is_empty() {
        return Err("public_code_search query must not be empty".to_string());
    }
    if input.query.chars().count() > MAX_QUERY_CHARACTERS {
        return Err(format!(
            "public_code_search query exceeds the {MAX_QUERY_CHARACTERS} character limit"
        ));
    }
    if !(1..=MAX_RESULTS).contains(&input.num_results) {
        return Err(format!(
            "public_code_search num_results must be between 1 and {MAX_RESULTS}"
        ));
    }
    if input.context_lines > MAX_CONTEXT_LINES {
        return Err(format!(
            "public_code_search context_lines must be between 0 and {MAX_CONTEXT_LINES}"
        ));
    }
    if input.query.contains('(') || input.query.contains(')') {
        return Err(
            "public_code_search query may not contain grouping parentheses; use simple terms and filters"
                .to_string(),
        );
    }
    for operator in ["and", "or", "not"] {
        if contains_operator(&input.query, operator) {
            return Err(format!(
                "public_code_search query may not contain the `{}` boolean operator",
                operator.to_ascii_uppercase()
            ));
        }
    }
    for filter in [
        "count",
        "timeout",
        "visibility",
        "context",
        "type",
        "select",
        "fork",
        "archived",
    ] {
        if contains_filter(&input.query, filter) {
            return Err(format!(
                "public_code_search query may not set the Horizon-owned `{filter}:` filter"
            ));
        }
    }
    Ok(())
}

fn contains_operator(query: &str, operator: &str) -> bool {
    let query = query.to_ascii_lowercase();
    query.match_indices(operator).any(|(offset, matched)| {
        let before = query[..offset].chars().next_back();
        let after = query[offset + matched.len()..].chars().next();
        before.is_none_or(|character| !is_query_word_character(character))
            && after.is_none_or(|character| !is_query_word_character(character))
    })
}

fn is_query_word_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn contains_filter(query: &str, name: &str) -> bool {
    let query = query.to_ascii_lowercase();
    let needle = format!("{name}:");
    query.match_indices(&needle).any(|(offset, _)| {
        let prefix = &query[..offset];
        let previous = prefix.chars().next_back();
        previous.is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_')
    })
}

#[derive(Default)]
struct RequestGate {
    next_start: Option<Instant>,
}

fn request_gate() -> &'static AsyncMutex<RequestGate> {
    static REQUEST_GATE: OnceLock<AsyncMutex<RequestGate>> = OnceLock::new();
    REQUEST_GATE.get_or_init(|| AsyncMutex::new(RequestGate::default()))
}

fn cooldown() -> &'static StdMutex<Option<Instant>> {
    static COOLDOWN: OnceLock<StdMutex<Option<Instant>>> = OnceLock::new();
    COOLDOWN.get_or_init(|| StdMutex::new(None))
}

fn note_cooldown(duration: Duration) {
    *cooldown()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now() + duration);
}

fn cooldown_remaining() -> Option<Duration> {
    let mut blocked_until = cooldown()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let remaining =
        blocked_until.and_then(|deadline| deadline.checked_duration_since(Instant::now()));
    if remaining.is_none() {
        *blocked_until = None;
    }
    remaining
}

async fn read_capped(response: &mut reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("public-code search response body failed: {error}"))?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(format!(
                "public-code search response exceeded the {limit} byte limit"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourcegraphMatch {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    repository: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    commit: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    chunk_matches: Vec<ChunkMatch>,
    #[serde(default)]
    line_matches: Vec<LineMatch>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChunkMatch {
    #[serde(default)]
    content: String,
    content_start: Position,
    #[serde(default)]
    best_line_match: Option<usize>,
}

#[derive(Default, Deserialize)]
struct Position {
    #[serde(default)]
    line: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineMatch {
    #[serde(default)]
    line: String,
    #[serde(default)]
    line_number: usize,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    #[serde(default)]
    done: bool,
    #[serde(default)]
    match_count: Option<usize>,
    #[serde(default)]
    skipped: Vec<Value>,
}

#[derive(Default, Deserialize)]
struct Alert {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
}

fn parse_stream(body: &[u8], max_results: usize) -> Result<CodeSearchResponse, String> {
    let text = std::str::from_utf8(body)
        .map_err(|_| "public-code search returned non-UTF-8 event data".to_string())?;
    let normalized = text.replace("\r\n", "\n");
    let mut results = Vec::new();
    let mut match_count = None;
    let mut truncated = false;
    let mut saw_done = false;
    let mut saw_completed_progress = false;
    let mut warnings = Vec::new();

    for block in normalized
        .split("\n\n")
        .filter(|block| !block.trim().is_empty())
    {
        let (event, data) = parse_event(block)?;
        match event {
            "matches" => {
                let matches: Vec<SourcegraphMatch> = serde_json::from_str(data)
                    .map_err(|error| format!("invalid public-code matches event: {error}"))?;
                append_matches(&mut results, matches, max_results);
            }
            "progress" => {
                let progress: Progress = serde_json::from_str(data)
                    .map_err(|error| format!("invalid public-code progress event: {error}"))?;
                if progress.done {
                    saw_completed_progress = true;
                    match_count = progress.match_count;
                    truncated |= !progress.skipped.is_empty();
                }
            }
            "alert" => {
                let alert: Alert = serde_json::from_str(data)
                    .map_err(|error| format!("invalid public-code alert event: {error}"))?;
                let message = match (alert.title.trim(), alert.description.trim()) {
                    ("", "") => "public-code search returned an alert".to_string(),
                    (title, "") => title.to_string(),
                    ("", description) => description.to_string(),
                    (title, description) => format!("{title}: {description}"),
                };
                truncated = true;
                if warnings.len() < MAX_WARNINGS {
                    warnings.push(truncate_chars(&message, MAX_WARNING_CHARACTERS));
                }
            }
            "done" => saw_done = true,
            "filters" => {}
            _ => {}
        }
    }
    if !saw_done {
        return Err("public-code search stream ended before the done event".to_string());
    }
    if !warnings.is_empty() && !saw_completed_progress {
        return Err(format!(
            "public-code search could not complete: {}",
            warnings.join("; ")
        ));
    }
    truncated |= match_count.is_some_and(|count| count > results.len());
    Ok(CodeSearchResponse {
        results,
        match_count,
        truncated,
        warnings,
    })
}

fn parse_event(block: &str) -> Result<(&str, &str), String> {
    let mut event = None;
    let mut data = None;
    for line in block.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            if event.replace(value.trim()).is_some() {
                return Err("public-code search event repeated its event field".to_string());
            }
        } else if let Some(value) = line.strip_prefix("data:") {
            if data.replace(value.trim()).is_some() {
                return Err("public-code search event repeated its data field".to_string());
            }
        } else if !line.starts_with(':') && !line.trim().is_empty() {
            return Err("public-code search returned a malformed event stream".to_string());
        }
    }
    match (event, data) {
        (Some(event), Some(data)) => Ok((event, data)),
        _ => Err("public-code search event omitted its event or data field".to_string()),
    }
}

fn append_matches(
    results: &mut Vec<CodeSearchResult>,
    matches: Vec<SourcegraphMatch>,
    max_results: usize,
) {
    for matched in matches {
        if results.len() >= max_results {
            break;
        }
        if matched.kind != "content"
            || !matched.repository.starts_with("github.com/")
            || matched.path.is_empty()
            || matched.commit.is_empty()
            || matched.repository.chars().count() > MAX_REPOSITORY_CHARACTERS
            || matched.path.chars().count() > MAX_PATH_CHARACTERS
            || matched.commit.chars().count() > MAX_COMMIT_CHARACTERS
        {
            continue;
        }
        let language = matched
            .language
            .and_then(|value| (value.chars().count() <= MAX_LANGUAGE_CHARACTERS).then_some(value));
        for chunk in matched.chunk_matches {
            if results.len() >= max_results {
                break;
            }
            results.push(CodeSearchResult {
                repository: matched.repository.clone(),
                path: matched.path.clone(),
                commit: matched.commit.clone(),
                language: language.clone(),
                line_number: chunk
                    .best_line_match
                    .unwrap_or(chunk.content_start.line)
                    .saturating_add(1),
                snippet: truncate_chars(&chunk.content, MAX_SNIPPET_CHARACTERS),
            });
        }
        for line in matched.line_matches {
            if results.len() >= max_results {
                break;
            }
            results.push(CodeSearchResult {
                repository: matched.repository.clone(),
                path: matched.path.clone(),
                commit: matched.commit.clone(),
                language: language.clone(),
                line_number: line.line_number.saturating_add(1),
                snippet: truncate_chars(&line.line, MAX_SNIPPET_CHARACTERS),
            });
        }
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.char_indices()
        .nth(max)
        .map_or_else(|| text.to_string(), |(end, _)| text[..end].to_string())
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
        seen: Mutex<Vec<CodeSearchRequest>>,
        response: Mutex<Option<Result<CodeSearchResponse, String>>>,
    }

    #[async_trait]
    impl CodeSearchAdapter for StubAdapter {
        async fn search(&self, request: CodeSearchRequest) -> Result<CodeSearchResponse, String> {
            self.seen.lock().unwrap().push(request);
            self.response.lock().unwrap().take().unwrap()
        }
    }

    fn valid_input() -> CodeSearchInput {
        CodeSearchInput {
            query: "VecDeque lang:rust repo:rust-lang/rust".to_string(),
            num_results: 3,
            context_lines: 2,
        }
    }

    #[test]
    fn request_appends_non_overridable_public_bounds() {
        let request = prepare_request(valid_input()).unwrap();
        assert!(request
            .backend_query
            .ends_with("(VecDeque lang:rust repo:rust-lang/rust)"));
        for constraint in [
            "context:global",
            "repo:^github\\.com/",
            "visibility:public",
            "type:file",
            "count:3",
            "timeout:10s",
            "fork:no",
            "archived:no",
        ] {
            assert!(request.backend_query.contains(constraint), "{constraint}");
        }
    }

    #[test]
    fn validation_rejects_bounds_unknown_fields_and_control_filters() {
        assert!(validate(&valid_input()).is_ok());
        for query in [
            "count:all VecDeque",
            "VecDeque TIMEOUT:1m",
            "VecDeque visibility:any",
            "VecDeque (type:symbol)",
            "(VecDeque)count:all",
            "VecDeque -fork:no",
            "VecDeque archived:yes",
            "VecDeque select:repo",
            "VecDeque OR repo:gitlab.com/gitlab-org/gitlab",
            "VecDeque not repo:github.com/rust-lang/rust",
            "(VecDeque) repo:rust-lang/rust",
        ] {
            assert!(
                validate(&CodeSearchInput {
                    query: query.to_string(),
                    ..valid_input()
                })
                .is_err(),
                "{query}"
            );
        }
        assert!(validate(&CodeSearchInput {
            query: "discount:price VecDeque".to_string(),
            ..valid_input()
        })
        .is_ok());
        assert!(validate(&CodeSearchInput {
            query: "ordinary words andromeda".to_string(),
            ..valid_input()
        })
        .is_ok());
        assert!(serde_json::from_value::<CodeSearchInput>(json!({
            "query": "VecDeque",
            "unknown": true
        }))
        .is_err());
        assert!(validate(&CodeSearchInput {
            num_results: MAX_RESULTS + 1,
            ..valid_input()
        })
        .is_err());
        assert!(validate(&CodeSearchInput {
            context_lines: MAX_CONTEXT_LINES + 1,
            ..valid_input()
        })
        .is_err());
    }

    #[test]
    fn parser_normalizes_chunk_matches_and_marks_incomplete_results() {
        let body = concat!(
            "event: ignored-future-event\n",
            "data: {\"ok\":true}\n\n",
            "event: matches\n",
            "data: [{\"type\":\"content\",\"repository\":\"github.com/rust-lang/rust\",",
            "\"path\":\"library/std/src/io/copy.rs\",\"commit\":\"abc123\",",
            "\"language\":\"Rust\",\"chunkMatches\":[{\"content\":\"before\\nVecDeque\\nafter\",",
            "\"contentStart\":{\"line\":127},\"bestLineMatch\":128}]}]\n\n",
            "event: progress\n",
            "data: {\"done\":true,\"matchCount\":4,\"skipped\":[{\"reason\":\"limit\"}]}\n\n",
            "event: done\n",
            "data: {}\n\n"
        );
        let parsed = parse_stream(body.as_bytes(), 10).unwrap();
        assert_eq!(parsed.results.len(), 1);
        assert_eq!(parsed.results[0].repository, "github.com/rust-lang/rust");
        assert_eq!(parsed.results[0].line_number, 129);
        assert_eq!(parsed.results[0].snippet, "before\nVecDeque\nafter");
        assert_eq!(parsed.match_count, Some(4));
        assert!(parsed.truncated);
    }

    #[test]
    fn parser_drops_content_matches_without_path_or_commit_attribution() {
        let body = concat!(
            "event: matches\n",
            "data: [{\"type\":\"content\",\"repository\":\"github.com/example/project\",",
            "\"path\":\"\",\"commit\":\"abc123\",\"chunkMatches\":[{",
            "\"content\":\"unattributed\",\"contentStart\":{\"line\":0}}]}]\n\n",
            "event: progress\n",
            "data: {\"done\":true,\"matchCount\":1,\"skipped\":[]}\n\n",
            "event: done\n",
            "data: {}\n\n"
        );
        let parsed = parse_stream(body.as_bytes(), 10).unwrap();
        assert!(parsed.results.is_empty());
        assert!(parsed.truncated);
    }

    #[test]
    fn parser_preserves_alerts_as_warnings_and_fails_on_malformed_or_incomplete_streams() {
        let alert = concat!(
            "event: progress\n",
            "data: {\"done\":true,\"matchCount\":0,\"skipped\":[]}\n\n",
            "event: alert\n",
            "data: {\"title\":\"No repositories found\",\"description\":\"narrow the query\"}\n\n",
            "event: done\n",
            "data: {}\n\n"
        );
        let parsed = parse_stream(alert.as_bytes(), 10).unwrap();
        assert!(parsed.truncated);
        assert_eq!(
            parsed.warnings,
            vec!["No repositories found: narrow the query"]
        );
        let fatal_alert = concat!(
            "event: alert\n",
            "data: {\"title\":\"Unable To Process Query\",\"description\":\"invalid regexp\"}\n\n",
            "event: done\n",
            "data: {}\n\n"
        );
        assert!(parse_stream(fatal_alert.as_bytes(), 10)
            .unwrap_err()
            .contains("Unable To Process Query"));
        assert!(parse_stream(
            b"event: matches\ndata: nope\n\nevent: done\ndata: {}\n\n",
            10
        )
        .is_err());
        assert!(parse_stream(b"event: filters\ndata: []\n\n", 10)
            .unwrap_err()
            .contains("before the done"));
    }

    #[tokio::test]
    async fn adapter_output_is_normalized_without_vendor_fields() {
        let adapter = StubAdapter {
            seen: Mutex::new(Vec::new()),
            response: Mutex::new(Some(Ok(CodeSearchResponse {
                results: vec![CodeSearchResult {
                    repository: "github.com/example/project".to_string(),
                    path: "src/lib.rs".to_string(),
                    commit: "abc123".to_string(),
                    language: Some("Rust".to_string()),
                    line_number: 7,
                    snippet: "fn example() {}".to_string(),
                }],
                match_count: Some(1),
                truncated: false,
                warnings: Vec::new(),
            }))),
        };
        let request = prepare_request(valid_input()).unwrap();
        let output = execute_with_adapter(&adapter, request).await.unwrap();
        assert_eq!(output["is_error"], false);
        assert_eq!(
            output["results"][0]["repository"],
            "github.com/example/project"
        );
        assert!(output.get("provider").is_none());
        assert!(output.get("trace").is_none());
        assert_eq!(adapter.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sourcegraph_adapter_uses_fixed_stream_contract() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut socket).await;
            let request = String::from_utf8(request).unwrap();
            let request_line = request.lines().next().unwrap();
            let target = request_line.split_whitespace().nth(1).unwrap();
            let url = Url::parse(&format!("http://local{target}")).unwrap();
            assert_eq!(url.path(), "/.api/search/stream");
            let pairs = url
                .query_pairs()
                .collect::<std::collections::HashMap<_, _>>();
            assert_eq!(pairs.get("v").map(|value| value.as_ref()), Some("V3"));
            assert_eq!(pairs.get("cm").map(|value| value.as_ref()), Some("true"));
            assert_eq!(pairs.get("display").map(|value| value.as_ref()), Some("3"));
            assert_eq!(pairs.get("cl").map(|value| value.as_ref()), Some("2"));
            assert!(pairs["q"].contains("visibility:public"));
            assert!(request
                .to_ascii_lowercase()
                .contains("accept: text/event-stream"));

            let body = concat!(
                "event: matches\n",
                "data: [{\"type\":\"content\",\"repository\":\"github.com/example/project\",",
                "\"path\":\"src/lib.rs\",\"commit\":\"abc123\",\"language\":\"Rust\",",
                "\"chunkMatches\":[{\"content\":\"fn example() {}\",",
                "\"contentStart\":{\"line\":6},\"bestLineMatch\":6}]}]\n\n",
                "event: progress\n",
                "data: {\"done\":true,\"matchCount\":1,\"skipped\":[]}\n\n",
                "event: done\n",
                "data: {}\n\n"
            );
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
        });

        let adapter = SourcegraphAdapter {
            client: Client::builder()
                .no_proxy()
                .redirect(redirect::Policy::none())
                .build()
                .unwrap(),
            endpoint: Url::parse(&format!("http://{addr}/.api/search/stream")).unwrap(),
        };
        let response = adapter
            .search(prepare_request(valid_input()).unwrap())
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].line_number, 7);
    }

    async fn read_http_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4_096];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            assert!(read > 0, "client closed before sending request headers");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                return request;
            }
        }
    }
}
