---
name: decision-record
description: Use before writing a code comment or design doc that explains a design choice, a rationale, or a rejected alternative - keeps policy out of comments and points decisions at the board. Trigger words: document a decision, why this design, rejected alternative, owner decision, design rationale, explain this choice in a comment.
---

# Recording decisions

A code comment or a design doc is not the record of a decision. It states what
the code does and why it is shaped that way **as engineering fact**. It does not
establish policy, and it does not say who decided.

## Rules

- **Do not write policy in a code comment.** State only what the next reader
  needs: what the code does, plus any constraint that cannot be inferred from
  the code itself (a wire lockstep, a provider-400 call/result pairing, a
  sandbox boundary). Do not write "the owner decided", "rejected design",
  "by design", "deliberately no", "we chose ... over ...", or a rationale that
  closes off an alternative the code has not actually closed.
- **Do not paraphrase a decision into docs.** A design doc describes the
  current design. It is not where a decision is made, attributed, or narrated.
- **The owner's decisions live on the board.** If a constraint genuinely comes
  from the owner, cite the board item. Do not restate it in the owner's voice,
  and do not write `(owner decision <date>)` — a date alone, or a self-declared
  attribution, is not a record and gets read later as authority it never had.
- **Rejected alternatives stay out.** Mention one only if the code cannot stand
  without the comparison, keep it to one line, and never cite a code comment as
  the canonical record of the decision.

## Why

A comment that reads as settled policy gets read by the next agent as something
the owner mandated, and it closes options that were never closed. Trimming the
prose is the fix; adding attribution labels is not — extra labels add text and
still do not make a decision traceable.

When in doubt: write the fact the code needs, and leave the decision to the
board.
