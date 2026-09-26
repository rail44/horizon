# Provider response lifecycle

One `ResponseAttempt` owns the request span and response collector. Closing it
returns either a completed response or a failure with its collected response.
The collector retains finalized tool requests and usage after disconnect,
timeout, or cancellation. Text is committed only after retry selection; a
failed attempt selected as the final outcome retains its observed partial text.
An error does not turn that partial text into a successful answer.

## Retry and completion

Retry classification remains at the provider boundary. Structured HTTP status
and Retry-After headers take precedence over the fallback for string-only
provider errors. Existing pacing is retained: three attempts for transient
500/502/503/504 and transport failures, indefinite cancellable pacing for 429,
and no timeout retry. An issued tool request prohibits retry because the host
may already have executed it. Text-only transport retries can repeat remote
generation or billing; this policy guarantees no repeated local tool effect,
not exactly-once remote inference.

The normalized finish reason takes precedence over token counts:

| Reason | Result |
| --- | --- |
| Stop / ToolCalls | Complete, unless streamed tools are unfinished. |
| Length | Truncated; existing bounded continuation applies. |
| ContentFilter | Refused; stop without automatic continuation. |
| Other | Unknown reason retained in the diagnostic; stop. |
| No reason in a final record | Existing token-cap heuristic plus unfinished-tool detection. |
| No final record | Unknown completion; stop. |

Each tool stream has its own name, byte count, flush clock, and receiving or
finalized state. Finalization closes the preview by its exact stream key before
publishing the corresponding request. Duplicate finalization and late chunks
cannot reopen it. A new provider attempt removes prior uncommitted text;
response-local folding prevents text and reasoning from accumulating across
attempts. Request completion removes any remaining unfinished previews.

## Settling issued tools

Stopping a response is separate from deciding what happened to its tools.
The provider sends an internal `SettleTools` barrier naming exact occurrences.
The host first folds already-queued worker results, then returns a matching
`ToolCallsSettled` receipt:

- Recorded results are returned unchanged, including successful writes.
- Approved retries are followed to their current occurrence.
- A denial held in a pending retry offer is retained; the unexecuted offer closes.
- Only unfinished occurrences receive cancellation. Late results cannot replace
  that terminal result. Cancellation does not roll back external side effects.

The provider waits for the receipt before closing a turn or starting recovery.
It consumes results into history once, including successful memory updates,
and removes duplicate queued notifications for that batch. User inputs remain
queued during the barrier. Runtime shutdown interrupts the wait and stops the
session rather than starting another request with unresolved history. A crashed
host still uses the existing persisted-history recovery path.

These coordination messages are excluded from client serialization and the
event log. Tool results themselves still use acknowledged persistence before
the receipt is sent. The provider never fabricates cancellations to stand in
for an unknown host result.

## Replay and compatibility

Within an explicitly delimited provider response, replay groups assistant text
and announced tools before their results. This handles a tool that finishes
before the stream announces a sibling tool. Raw JSONL order remains unchanged.
Histories without provider-request markers retain the existing pairing repair.

Agent wire v26 adds keyed preview closure. A full shell and agentd restart is
required to activate it; reloading only the daemon cannot update the shell's
protocol. The terminal and board protocols, event-log v3, and stored result
bodies are unchanged. No history conversion is required for this change.

## Verification

Local HTTP/SSE fixtures exercise both configured adapters through normal
completion, disconnect after tool emission, idle timeout, cancellation,
text-only retry, prohibition of retry after tool emission, finish-reason
precedence, and multiple tools. OpenAI argument fragments are interleaved;
Anthropic tool blocks follow its ordered content-block format.

A real filesystem write followed by a stopped batch is checked through host
settlement, provider history, JSONL, DuckDB restoration, and the changes view.
Additional tests cover exact occurrence selection, completed and pending
retries, preview closure, and frame reconstruction across retries.
