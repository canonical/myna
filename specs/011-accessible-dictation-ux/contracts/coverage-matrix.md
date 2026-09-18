# Contract: State-to-channel coverage matrix (SC-002, FR-032)

## Source of truth

`extensions/myna-shell/coverage-matrix.json` (see data-model.md) is the single
checked-in file. Both languages load it directly (Rust: `include_str!`-style
path relative to the workspace root in `client/myna-desktop/tests/coverage.rs`;
GJS: `imports` the JSON in `test/coverage.test.js`) — no generated or
duplicated copy.

## Guarantees

| ID | Guarantee | Test tier |
|---|---|---|
| C1 | Every state entry has a non-empty `channels.visual` array. | hermetic, both languages |
| C2 | Every state entry has a non-empty `channels.non_visual` array. | hermetic, both languages |
| C3 | No entry has `colour_only: true` or `sound_only: true`. | hermetic, both languages |
| C4 | Every `DictationState`/`IndicatorState` variant that exists in Rust has a matching entry by `id`; adding a new state without a matching matrix entry fails the build. | hermetic (Rust, exhaustive match against the enum) |
| C5 | Every `states.js` descriptor id has a matching matrix entry; same exhaustiveness check on the GJS side. | hermetic (GJS) |

## Contrast regression gate (FR-013, FR-032)

A small hermetic Rust utility computes the WCAG relative-luminance contrast
ratio for every foreground/background colour pair declared in the shipped
stylesheet (parsed as literal hex/rgb values, not rendered) and asserts:

*As implemented this is `myna_hud::contrast`, reading
`client/myna-hud/src/style.css` through `include_str!` so the check cannot
drift from the file it describes. It was originally specified against
`extensions/myna-shell/stylesheet.css`; that stylesheet was removed when the
HUD became the standalone `myna-hud` renderer, and the gate was re-pointed
(tasks.md T084) rather than left guarding a file nothing renders.*

| ID | Guarantee |
|---|---|
| K1 | Text colour pairs meet ≥4.5:1. |
| K2 | Non-text meaningful element colour pairs meet ≥3:1. |

This runs without a display or compositor (FR-030) and fails the build the
moment an authored colour value regresses below threshold.
