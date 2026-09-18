# Contract: Plain-language failure mapping (US4, FR-023–027)

## `FailurePresentation` registry

One entry per known ad-hoc failure source today (`controller.rs`'s
`OrchestratorEvent::Error` messages, `inject::InjectError` variants:
`SecureField`, `NoTarget`, `Unavailable`, `Backend`), each authored once in
`client/myna-desktop/src/failure.rs`:

| ID | Guarantee | Test tier |
|---|---|---|
| F1 | Every known failure source maps to exactly one `FailurePresentation` with a non-empty plain-language `message` containing no error codes, internal component names, or jargon as primary text (FR-023). | hermetic |
| F2 | The same `FailurePresentation` is used to render the indicator's error state, the `notify-rust` toast, `myna-cli`'s stderr line, and the AT-SPI announcement — asserted by a single shared fixture feeding all four render paths in one test (FR-024). | hermetic |
| F3 | Two different `FailurePresentation`s for the same underlying concept (e.g. two call sites both meaning "no microphone") never diverge in wording — enforced by keying presentations by a stable `id`, not by ad-hoc string matching at each call site (FR-024a). | hermetic |
| F4 | `severity: Critical` presentations persist until acknowledged; `severity: Recoverable` presentations auto-dismiss but remain queryable afterward via `myna-desktop`'s existing state/notice history (FR-025, FR-026). | hermetic |
| F5 | A long-running operation (e.g. model loading) past its threshold produces an actionable `FailurePresentation`-shaped message distinguishable from ordinary progress (FR-027). | hermetic (fake clock) |

## Non-goals

This contract does not define or depend on the T31 wire-level error-code
taxonomy or T62's UX mapping; it is a compatible adapter over today's ad-hoc
strings per spec Assumptions, and is designed so a future taxonomy can populate
the same `FailurePresentation` registry without changing this contract's
guarantees.
