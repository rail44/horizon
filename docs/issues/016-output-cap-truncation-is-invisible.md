---
id: 016
title: A turn truncated at the output-token cap ends as "Completed" with nothing to show for it
status: partially-resolved
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

## Resolution (surfacing half)
Fixed in `dcc305f` (merged as `aeb472e`). `output_cap_truncated` compares
the reported `output_tokens` against the cap actually sent, excluding
cancelled turns; a match raises an `Event::Error` naming both numbers and
routes the turn through the *existing* truncation recovery — same guard,
same three-attempt ceiling as the mid-stream tool-call case, with a
continuation prompt that tells the model to act rather than re-derive its
plan. No UI change was needed: the error rides the generic
`AgentFrameItem::Error` rendering the other case already used.

The two blind spots are in the code's doc comment, not just here: usage
never arrives on ~2% of `syn:large:text` streams, and a provider that
reports zero usage is indistinguishable from a genuinely tiny turn.

## The cause half is NOT resolved
Probed against the configured provider 2026-08-04 (`syn:large:text` on
synthetic.new, trivial and reasoning-heavy prompts, n=1 per condition):

| setting | reasoning chars | answer chars | completion tokens |
|---|---|---|---|
| baseline | 2,814 | 650 | 804 |
| `none` / `low` | 0 | 4,652 | 1,073 |
| `medium` | 1,910 | 613 | 589 |
| `high` | 1,611 | 1,175 | 705 |

So `reasoning_effort` **is** honoured — but it is effectively binary
(`none` and `low` both suppress entirely; `medium` and `high` do not
differ), and suppressing the reasoning block does not reduce the thinking,
it **relocates it into the answer text**: total output went *up* 33%.
That is why the knob fixed the judge (a one-token classifier, where
relocation into the answer is exactly what was wanted) and would not fix
an implementation turn.

Adding "work incrementally" instructions to the prompt or brief was
considered and declined by the owner (2026-08-04) as bad know-how — the
goal is that a simple brief suffices, not that each incident adds a line.

Remaining observations, recorded without a causal claim (the owner has
not agreed that the model is the problem, and the cross-model numbers
are confounded — `syn:large:text` carried the implementation-heavy
dogfood workload while other models' samples come from different eras
and task types): in this corpus `syn:large:text`'s reasoning tail
measured 3.5-5x the other two models at p99 and both capped turns were
its; and the delegation prompt section's plan-first framing has only
ever been measured for delegation adoption, never for its effect on
post-report re-planning. Any attribution needs a controlled comparison
first.
