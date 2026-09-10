//! One-off LLM calls that distill an agent session's first user message
//! into a short tab title.
//!
//! This is the title-summarizing sibling of `judge`'s auxiliary-model
//! plumbing: same provider (the OpenAI-completions client over
//! `OPENAI_API_KEY`), same pooled-client discipline (`shared_client`'s
//! process-wide `OnceLock`, so the ~0.5s warm-connection latency is paid
//! once per process, not per call), same dedicated lazily-started tokio
//! runtime so a caller's own thread never blocks on the network. It
//! deliberately does **not** share `judge`'s client internals: those are
//! `pub(super)`-scoped to the approval gate's seam (`ModelClient` and its
//! mock), and a title call needs none of that mockability -- every
//! failure path is a graceful `None` that leaves the raw first-message
//! title showing.
//!
//! The default model id is [`crate::config::DEFAULT_JUDGE_MODEL`]
//! (`syn:small:text`, the provider-maintained small-model alias) for the
//! same reason the judge uses it: a tab title is a cheap auxiliary task
//! that must never require the acting model. It is overridable through
//! [`TITLE_MODEL_VAR`], its own env var -- not the judge's, so tuning the
//! approval gate's model never silently retitles tabs.
//!
//! Sync by design: [`summarize_session_title`] blocks its calling thread
//! for at most [`TITLE_TIMEOUT`] and is meant to be invoked from a
//! background thread (the shell calls it under
//! `cx.background_executor().spawn`), never from the UI thread or a
//! session pump's own thread.

use std::sync::OnceLock;
use std::time::Duration;

use rig_core::client::CompletionClient;
use rig_core::completion::{AssistantContent, CompletionModel, Message};
use rig_core::providers::openai;

use crate::config;

/// Overrides the title summarizer's model id. Env-only (mirroring
/// `HORIZON_AGENT_JUDGE_MODEL`'s treatment -- the config-file surface is
/// frozen), falling back to [`crate::config::DEFAULT_JUDGE_MODEL`].
/// Deliberately *not* the judge's override var: the two features tune
/// independently.
pub const TITLE_MODEL_VAR: &str = "HORIZON_AGENT_TITLE_MODEL";

/// How long one summarizer call may take before it gives up and the raw
/// first-message title stays. A tab title is never worth more than a few
/// seconds of waiting (the judge's 60s budget is for approval decisions
/// that gate real work; this gates nothing).
const TITLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap on how much of the first user message is sent, in chars: plenty
/// for "what this session is about", small enough that a pasted log
/// dump cannot blow up the request.
const MAX_PROMPT_CHARS: usize = 2_000;

/// The reply is 3-8 words; 32 completion tokens is generous headroom even
/// for CJK tokenization, and caps the damage of a chatty model.
const MAX_TOKENS: u64 = 32;

const SYSTEM_PROMPT: &str = "You write concise tab titles for a coding-agent UI. \
From the user's message, reply with ONLY the title: 3-8 words, in the same \
language as the message, no quotes, no trailing punctuation, no explanation.";

/// The system prompt, exposed for tests' sake -- it is the entire
/// "contract" the reply's shape depends on.
#[cfg(test)]
pub(crate) fn system_prompt() -> &'static str {
    SYSTEM_PROMPT
}

/// Pure precedence for the summarizer's model id: [`TITLE_MODEL_VAR`]
/// wins, else the sanctioned small-model alias.
fn resolve_title_model(env_value: Option<String>) -> String {
    env_value.unwrap_or_else(|| config::DEFAULT_JUDGE_MODEL.to_string())
}

/// The user-side prompt: the first message, char-truncated (not
/// byte-truncated -- CJK text must not lose half a character).
fn prompt_input(first_message: &str) -> String {
    first_message.chars().take(MAX_PROMPT_CHARS).collect()
}

/// Strips the wrapper whitespace/quotes a small model loves to add; `None`
/// when nothing usable remains (an empty or quote-only reply).
fn clean_title(text: &str) -> Option<String> {
    let trimmed = text
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Lazily-started shared runtime for title calls -- the judge runtime's
/// twin (`judge::runtime`), under its own thread name so a stuck title
/// call is identifiable in a sample. One worker thread is plenty: calls
/// are network-bound and rare (one per agent session attach).
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("horizon-agent-title")
            .enable_all()
            .build()
            .expect("failed to build the shared title-summarizer runtime")
    })
}

/// Process-wide pooled client, built only when `OPENAI_API_KEY` is set --
/// same shape and rationale as `judge::client`'s `shared_client` (which
/// this cannot reuse: that one is `pub(super)` and carries the judge's
/// own error text).
fn shared_client(base_url: Option<&str>) -> anyhow::Result<openai::CompletionsClient> {
    static CLIENT: OnceLock<Option<openai::CompletionsClient>> = OnceLock::new();
    CLIENT
        .get_or_init(|| build_client(base_url))
        .clone()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "title summarizer client unavailable ({} unset, or client build failed)",
                config::OPENAI_API_KEY_VAR
            )
        })
}

fn build_client(base_url: Option<&str>) -> Option<openai::CompletionsClient> {
    let api_key = std::env::var(config::OPENAI_API_KEY_VAR).ok()?;
    let mut builder = openai::CompletionsClient::builder().api_key(&api_key);
    if let Some(base_url) = base_url {
        builder = builder.base_url(base_url);
    }
    builder.build().ok()
}

/// Asks the small model for a concise tab title for a session whose first
/// user message is `first_message`. Blocking (runs to completion on the
/// dedicated runtime, bounded by [`TITLE_TIMEOUT`]); call from a
/// background thread. `base_url` follows the same resolution the agent
/// runtime uses (`OPENAI_BASE_URL` env, else the config file's
/// `[provider].base_url`; `None` = rig's own default) -- the caller
/// resolves it, since this crate never reads the config file.
///
/// `None` on every failure -- no API key, client build failure, timeout,
/// transport error, empty/quote-only reply -- never `Err`: the caller's
/// fallback (keep the raw first-message title) is always the right move,
/// and there is nothing to distinguish between the failure modes.
pub fn summarize_session_title(base_url: Option<&str>, first_message: &str) -> Option<String> {
    std::env::var_os(config::OPENAI_API_KEY_VAR)?;
    let model = resolve_title_model(std::env::var(TITLE_MODEL_VAR).ok());
    let user_content = prompt_input(first_message);
    runtime().block_on(async move {
        let client = shared_client(base_url).ok()?;
        let completion_model = client.completion_model(&model);
        let response = tokio::time::timeout(TITLE_TIMEOUT, async move {
            completion_model
                .completion_request(Message::user(user_content))
                .preamble(SYSTEM_PROMPT.to_string())
                .max_tokens(MAX_TOKENS)
                // `reasoning_effort: none`, the judge's hard-won lesson
                // (`judge::client`): a reasoning-first model burns its
                // whole token budget in its think block and never emits
                // the answer -- the exact failure mode this one-shot
                // call cannot afford.
                .additional_params(serde_json::json!({ "reasoning_effort": "none" }))
                .send()
                .await
        })
        .await
        .ok()?
        .ok()?;

        let text = response
            .choice
            .into_iter()
            .find_map(|content| match content {
                AssistantContent::Text(text) => Some(text.text),
                _ => None,
            })
            .unwrap_or_default();
        clean_title(&text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_resolution_prefers_the_env_override() {
        assert_eq!(resolve_title_model(Some("m:test".into())), "m:test");
        assert_eq!(
            resolve_title_model(None),
            config::DEFAULT_JUDGE_MODEL.to_string()
        );
    }

    #[test]
    fn prompt_input_truncates_by_chars_not_bytes() {
        // 2 * MAX_PROMPT_CHARS Japanese chars: a byte cut would end
        // mid-character (each char is 3 bytes); the char cut keeps whole
        // characters and exactly MAX_PROMPT_CHARS of them.
        let long = "あ".repeat(MAX_PROMPT_CHARS * 2);
        let input = prompt_input(&long);
        assert_eq!(input.chars().count(), MAX_PROMPT_CHARS);
        assert!(input.ends_with('あ'));
    }

    #[test]
    fn prompt_input_keeps_short_messages_verbatim() {
        assert_eq!(prompt_input("hello"), "hello");
    }

    #[test]
    fn clean_title_strips_wrapping_quotes_and_whitespace() {
        assert_eq!(
            clean_title(" \"Fix login bug\" "),
            Some("Fix login bug".into())
        );
        assert_eq!(clean_title("'タブ修復'"), Some("タブ修復".into()));
        assert_eq!(clean_title("`a b`"), Some("a b".into()));
    }

    #[test]
    fn clean_title_rejects_nothing_left() {
        assert_eq!(clean_title(""), None);
        assert_eq!(clean_title("   "), None);
        assert_eq!(clean_title("\"\""), None);
        assert_eq!(clean_title("''"), None);
    }

    #[test]
    fn clean_title_leaves_inner_quotes_alone() {
        assert_eq!(
            clean_title("Saying \"hi\" loudly"),
            Some("Saying \"hi\" loudly".into())
        );
    }

    #[test]
    fn system_prompt_demands_a_bare_short_title() {
        // The reply shape the caller relies on (trim + quote strip is all
        // the parsing there is) -- keep the prompt demanding exactly that.
        let prompt = system_prompt();
        assert!(prompt.contains("ONLY the title"));
        assert!(prompt.contains("3-8 words"));
        assert!(prompt.contains("same language as the message"));
    }
}
