# Reviewing refactoring opportunities

Use [refactor-audit](../scripts/refactor-audit/README.md) to collect repeatable
signals across the codebase. These signals choose where to read; they do not
decide what to refactor. Human and agent reviewers use the same evidence.

1. **Check coverage.** Read the report status, selected roots, test partition,
   exclusions, and limitations. Resolve analysis failures before interpreting
   absent results. Also sample ordinary code outside the rankings: misplaced
   responsibility can have low complexity and no clones.
2. **Follow a responsibility.** Read callers, callees, relevant design docs,
   and tests around each entry point. Identify the owned state, operation's
   contract, reasons it changes, and dependencies. A proposed boundary should
   reduce independent responsibilities or contain a concrete change.
3. **Compare peers.** For code serving the same role, compare lifecycle,
   timeouts, errors, cancellation, ordering, visibility, and command routing.
   Cite the existing convention. Distinguish deliberate domain differences
   from accidental variation; repeated syntax alone is insufficient.
4. **Inspect readability.** Look for unnecessary branching, indirection, hidden
   state changes, and misleading naming. Explain the reading or modification
   made simpler. Extraction that only moves lines is not an established benefit.
5. **Record a judgment.** Keep one short entry per responsibility below.
   Consult co-change or dependency graphs when a boundary remains unclear.
   A recurring, confirmed convention can become a scoped ast-grep rule with
   positive and negative examples.

```text
Location / responsibility:
Evidence: source ranges, peer comparison, relevant convention or design doc
Maintenance cost: concrete change or reading task made difficult
Required differences / counterevidence:
Judgment: refactor / preserve / investigate (and why)
If refactoring: intended boundary and behavior to preserve
```

Keep raw measurements in the generated report. Record accepted architectural
decisions in the owning design document. This guide does not authorize product
changes or define a project-wide development workflow.

For recurring reviews, use the audit tool's `record` command and pass the ledger
with `scan --reviews`. Include related source files that informed the judgment;
changed or absent evidence requires another review. Keep reasons visible rather
than suppressing findings. Use `compare --mapping` for moves or extractions so
the comparison includes the helpers and retains unmatched functions.
