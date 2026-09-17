# Contract: Screen-reader-safe terminal output (US5, FR-028/029)

`myna-cli`'s `myna-dictate` binary.

| ID | Guarantee | Test tier |
|---|---|---|
| T1 | With colour/emoji disabled (`NO_COLOR`/non-tty stdout, or an explicit flag), every state transition, transcript result, and failure is fully distinguishable from explicit textual markers alone (e.g. `[listening]`, `[error]`), never from colour or emoji alone (FR-028). | hermetic (capture stdout, assert on plain text) |
| T2 | Output is line-oriented and append-only while a session runs: no in-place cursor redraw, no spinner animation frames repeated on the same line (FR-029). | hermetic (assert each write ends in `\n`, no `\r`/ANSI cursor-movement sequences) |
| T3 | Failures are written to stderr using the same `FailurePresentation` text used by the other surfaces (contracts/failure-mapping.md F2), not a separate ad-hoc CLI-only string (FR-029). | hermetic (shared fixture) |
