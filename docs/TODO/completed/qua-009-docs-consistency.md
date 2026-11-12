# QUA-009 — Documentation and code consistency sweep

**Status: Done.** The architecture and persistence documentation was updated to
describe the PostgreSQL snapshot store, `--state-postgres`, and the remaining
file-only behavior. README progress/count statements and the backlog gap
register were reconciled with the implementation. CI derives migration
expectations from the checked-in migration head and runs live PostgreSQL and
Markdown link checks.

The Markdown checker validates inline relative links and images, ATX heading
anchors (including duplicate headings), and explicit HTML anchor IDs. It skips
code blocks and external URLs. It does not parse reference-style links,
setext headings, nested parentheses in destinations, or anchors in non-Markdown
targets. Keep those limitations in mind during manual doc review.

The original acceptance checklist remains in
[`70-testing-quality.md`](../70-testing-quality.md#qua-009--documentation-and-code-consistency-sweep).

**Metadata refresh (2026-11-12):** The workspace added its seventeenth crate
and accepted ADRs 0032–0033 after the original sweep. README counts and the ADR
range were refreshed; `python scripts/check_readme_metadata.py` passes.
