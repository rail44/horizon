---
id: 016
title: A turn truncated at the output-token cap ends as "Completed" with nothing to show for it
status: open
severity: high
area: agent, ui
---

## Repro
Observed 2026-08-03 on dogfood session `0623ff55` (T-gitsegments), a task
with many edit sites (a type change plus classification, dispatch, tests
and a design doc). One turn:

- `provider_request_usage`: `output_tokens: 32768` — exactly
  `DEFAULT_AGENT_MAX_OUTPUT_TOKENS` (`crates/horizon-agent/src/config.rs`)
- of that, 128,800 characters arrived as `reasoning_delta`; assistant
  text: **zero**; tool calls: **zero**
- the reasoning ends mid-enumeration ("Edit 6: update the tests. Let me
  rewrite the first one:") — it is not a finished thought
- `turn_ended: "Completed"`, and the session went to `WaitingForUser`

The model had planned the whole implementation internally, down to
"cargo.rs line 31: `ShellToken::Boundary` → `ShellToken::Boundary(_)`",
and ran out of output budget before emitting a single tool call.

## Observed
From the UI, and from the event log, this is indistinguishable from a
turn that simply had nothing to do. Nothing says "truncated". The owner
could not tell from the screen what had happened; the integrator only
established it after the fact by comparing `output_tokens` against the
cap constant, checking that no other request in the session came near
that number, and noticing the reasoning ended mid-sentence.

Contrast the *other* truncation, which the same session hit later:
a tool call cut off mid-stream raises "Provider truncated 1 tool call(s)
mid-stream", surfaces as an `error` event, and auto-continue recovers.
That one leaves a structural trace — a call that started and never
finalized. Hitting the cap during reasoning leaves no such trace, so
today Horizon detects one of its two truncation modes and is blind to
the other.

## Expected
A turn that ends because it ran out of output budget says so — in the
UI, and as an event a reader can find later. Recovery can stay manual;
the minimum is that the user is not left inferring it from token
arithmetic.

## Notes
`provider_request_finished` currently carries `provider_payload: null`;
no finish/stop reason is recorded anywhere, and a grep for
`finish_reason`/`stop_reason` in `providers/rig/` finds nothing. Whether
the provider supplies one through rig's streaming API is the first thing
to check — a recorded `stop_reason == "length"` would be a far better
signal than inferring truncation from `output_tokens == cap`, which is
only *almost* conclusive (a turn could legitimately end at exactly the
cap, though nothing else in this session came within 12k of it).

Two separable pieces of work:

1. **Surfacing** — detect the cap-truncation, emit an event for it, show
   it in the agent pane. Consider whether auto-continue should extend to
   this case, or whether resuming a turn whose reasoning was lost is
   worse than reporting it.
2. **Cause** — why is a turn spending 32k output tokens on reasoning with
   zero tool calls in the first place? This session's model
   (`syn:large:text`, GLM) produces reasoning volume out of proportion to
   its output on this workload; the integrator has repeatedly seen
   `reasoning_delta` dominate the event log. Whether that is promptable
   (brief phrasing, task granularity), configurable (a provider-side
   reasoning-effort control), or inherent to the model is unknown.

Workaround that unblocked the session: telling it to make one edit and
immediately call the tool, deciding the next edit only after seeing the
result. Its following turns were normal (5395, 1831, 76, 10413 output
tokens).
