# Agent pane Floem/Rig API notes

This note records the API facts that should constrain the next Agent pane UI
implementation. It is intentionally a research note before another spike.

## Floem findings

`scroll(child)` is the correct owner for viewport scrolling. Floem's scroll view
handles `PointerWheel`, updates `child_viewport`, and paints the child with a
negative viewport offset. If `overflow_clip(true)` is enabled, painting is
clipped to the scroll view rect before drawing the child.

`ScrollCustomStyle::shrink_to_fit()` is relevant for Agent pane content because
it sets `min_size(0, 0)` and `size_full()`. This lets the scroll viewport shrink
inside the pane instead of forcing the transcript's full content height into the
layout.

Agent pane code should not intercept wheel events at the pane level when the
transcript is inside Floem `scroll`. A parent handler that stops
`PointerWheel` prevents the scroll view from receiving the wheel event.

`clip(child)` is only a paint clipping wrapper. It does not provide scroll state
or wheel handling, so it is not a substitute for transcript scrolling.

Floem provides `dyn_stack(each_fn, key_fn, view_fn)` for reactive lists. This is
a better fit than a fixed tuple of transcript rows because agent history grows
over time and rows have variable heights. `virtual_stack`/`virtual_list` are for
large lists with known or computable item sizes; that is probably premature for
the first Agent transcript implementation because assistant Markdown blocks have
variable height.

Floem text style defaults matter:

- `FontFamily` is inherited and should come from the same Horizon font constant
  used by terminal UI.
- Text overflow defaults to wrapping. Transcript content should avoid
  `text_ellipsis()`/`text_clip()` unless the row is intentionally summarized.

## Rig findings

Rig models assistant output as ordered `AssistantContent`:

- `Text(Text)`
- `ToolCall(ToolCall)`
- `Reasoning(Reasoning)`
- `Image(Image)`

`Reasoning` contains provider-specific reasoning blocks and exposes
`display_text()` for visible text/summary-like content. It is not just a
transient UI delta.

OpenAI Responses API conversion in `rig-core` preserves structured output order
from `response.output`. When a provider returns top-level string reasoning and
no structured reasoning item, Rig prepends it before the output content. Rig's
own tests assert that top-level reasoning appears before text in that case.

For multi-turn replay, Rig treats reasoning as a first-class assistant content
item. Structured reasoning with an id can be replayed as a reasoning input
item; idless or unreplayable reasoning may be omitted from the next request
while preserving assistant text.

Implication for Horizon: `Reasoning` should be represented as a distinct
transcript block, using `ReasoningDelta` for streaming updates. Assistant
response text should use `AssistantTextDelta` while streaming and
`MessageCommitted` when the final transcript item is stable.

## Current Horizon gaps

The current Agent view still uses a fixed eight-row projection:

```rust
pub const MAX_AGENT_ROWS: usize = 8;
```

That makes the transcript a window over items rather than a scrollable document.
It also keeps a legacy item-count based `scroll_offset`, which does not match
Floem's actual pixel viewport model.

The current view also truncates some content:

```rust
const MAX_VISIBLE_CHARS: usize = 720;
```

This directly conflicts with the requirement that long assistant replies are
not omitted. Tool output can be summarized, but assistant/user text should be
complete inside the scroll viewport.

The current UI estimates row height from character counts. That is brittle for
Markdown, Japanese text, variable fonts, and any future rich content. The next
version should let Floem lay out dynamic rows naturally inside a scroll view.

The old `MessageDelta` path was used for Rig reasoning:

```rust
AssistantContent::Reasoning(reasoning) => AgentEvent::MessageDelta(...)
```

That was the wrong domain model. Thinking/reasoning should be a labeled,
collapsible transcript block backed by `ReasoningDelta`. Assistant body text
should be separate, backed by `AssistantTextDelta` during streaming.

## Design guidance

Use a transcript projection layer between `AgentFrameItem` and Floem views.
Suggested block kinds:

- `UserMessage`
- `AssistantMessage`
- `Thinking`
- `ToolCall`
- `ToolResult`
- `Approval`
- `EphemeralStatus`
- `Error`

The projection should preserve turn order and should keep `Thinking` as the
step before the assistant answer when Rig/OpenAI gives that relationship. The
UI can render the thinking block as a compact row labeled `thinking`, toggling
open to reveal the full content. It should not disappear, and it should not be
moved after the answer unless the provider content sequence actually says so.

Use `scroll(dyn_stack(...))` for the transcript. The scroll view should have
`shrink_to_fit()` and `overflow_clip(true)`, and the composer should be outside
the scroll view in the pane's vertical layout so chat history cannot paint
behind it.

User and assistant messages should be distinguished by alignment and color, not
by visible `user`/`agent` labels. Tools, approvals, and thinking can keep labels
because they are process/status blocks rather than conversational speakers.

Markdown should be treated as a real rendering requirement. A lightweight first
step can project Markdown into block labels for headings, paragraphs, lists, and
code blocks. Removing `**` or heading markers from plain text is not enough.

## References

- Floem scroll: `/home/satoshi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/floem-0.2.0/src/views/scroll.rs`
- Floem clip: `/home/satoshi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/floem-0.2.0/src/views/clip.rs`
- Floem dynamic stack: `/home/satoshi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/floem-0.2.0/src/views/dyn_stack.rs`
- Floem virtual stack: `/home/satoshi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/floem-0.2.0/src/views/virtual_stack.rs`
- Rig message model: `/home/satoshi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rig-core-0.39.0/src/completion/message.rs`
- Rig completion response: `/home/satoshi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rig-core-0.39.0/src/completion/request.rs`
- Rig OpenAI Responses conversion: `/home/satoshi/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rig-core-0.39.0/src/providers/openai/responses_api/mod.rs`
- Horizon Rig bridge: `src/agent_rig_spike.rs`
- Horizon Agent view: `src/agent_view.rs`
