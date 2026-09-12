# Contract: `Injector` (text-injection seam) + IBus backend

**Feature**: 003-desktop-injection · **Crate**: `client/myna-desktop`

The injection boundary (T22, UD129 Text Injection Layer). Backend-agnostic
(FR-016); `IbusInjector` is the shipped implementor, `MockInjector` the test
fixture. This contract restates the guarantees so they can be encoded as
executable tests (TDD, Principle I) before the code.

## Interface (new)

```rust
pub trait Injector: Send {
    /// Bind the surface focused *now* as the session target.
    async fn acquire(&mut self) -> Result<InjectionTarget, InjectError>;
    /// Reflect recording/transcription activity where the backend supports it.
    async fn set_activity(&mut self, active: bool);
    /// Insert stable committed text (never modified afterwards). Commit-only.
    async fn commit(&mut self, text: &str) -> Result<(), InjectError>;
    /// Streaming preedit (R9, landed 2026-07-27 as opt-in `--preedit`): render a
    /// volatile in-flight hypothesis in the target's preedit region, replaced on
    /// the next call and cleared by `commit`. Default no-op; only backends with
    /// a real preedit region honor it. Called by the controller only when the
    /// opt-in is on AND `supports_preedit()`; the commit-only default never
    /// routes unstable text here.
    async fn set_preedit(&mut self, _text: &str) {}
    /// Whether this backend has a replacement-safe preedit region (IBus / future
    /// Wayland input-method-v2 → true; uinput/wtype fallback → false).
    fn supports_preedit(&self) -> bool { false }
    /// Abort without injecting anything further. Idempotent.
    async fn cancel(&mut self);
    /// Finalize and release the target/engine. Idempotent.
    async fn end(&mut self);
    /// Focus/target-loss events for the acquired target.
    fn focus_events(&mut self) -> BoxStream<'_, FocusEvent>; // FocusOut | TargetGone
}

pub struct InjectionTarget { /* opaque; carries secure flag + identity */ }
pub enum FocusEvent { FocusOut, TargetGone }
pub enum InjectError { SecureField, NoTarget, Unavailable(String), Backend(String) }
```

## Guarantees (each row → at least one test)

| # | Given | When | Then | Spec |
|---|-------|------|------|------|
| I1 | a focused editable target, engine reachable | `acquire()` then `commit("hello")` then `end()` | "hello" is inserted into that target; the transcript matches; nothing typed elsewhere | FR-011, SC-001, US1-1 |
| I2 | several committed segments | each `commit(seg)` in order | each segment inserted once, in order, never modified after | FR-012, US1-2 |
| I3 | an unstable hypothesis (`Snippet`) | controller receives it | `commit` is **not** called (commit-only) | FR-012, SC-006, US1-4 |
| I4 | a session with no recognized speech | `end()` with no `commit` | target unchanged; nothing inserted | US1-5 |
| I5 | a focused **password/secure** field | `acquire()` | `Err(SecureField)`; no engine activation persists | FR-021, SC-008, US4-3 |
| I6 | no editable surface focused | `acquire()` | `Err(NoTarget)` (clear failure, not silent no-op) | FR-023 |
| I7 | IBus not reachable | `acquire()`/`commit()` | `Err(Unavailable(..))`; no silent text loss | FR-023 |
| I8 | an acquired target, focus moves away | during the session | a `FocusEvent::FocusOut` is emitted (controller ends safely) | FR-014, SC-007, US4-1 |
| I9 | an acquired target whose window closes | mid-session | a `FocusEvent::TargetGone` is emitted | FR-022, US4-2 |
| I10 | any commit | — | only literal text is sent; no Tab/Alt+Tab/Super/F-key synthesized | FR-015, US4-5 |
| I11 | `cancel()` / `end()` called twice, or after an error | — | idempotent; the prior IME/global-engine is restored exactly once | FR-013, R1 |
| I12 | any session | injector runs | no audio touched; no transcript content logged by default | FR-024, Principle V |

## Test homes

- **Hermetic (no IBus/D-Bus)**: I2, I3, I4, I10, I11, I12 and the *shape* of I1/
  I5–I9 are covered against `MockInjector` (scripted `acquire` outcomes + focus
  events; recorded `commit`/`cancel`/`end`). Green offline, no display.
- **Integration (env-gated `MYNA_IBUS_TESTS=1`, real IBus daemon)**: I1, I5, I8,
  I9, I11 (global-engine set/restore) against a live engine committing into a
  headless GTK/Qt test entry and a scripted password field. Runs on the desktop
  VM and on hardware unchanged (Principle II). `tests/ibus_hw.rs`.

## Non-goals (this iteration)

- ~~No preedit/provisional rendering in the target — **commit-only MVP**.~~
  **Updated 2026-07-27**: preedit rendering landed as the opt-in
  `myna-desktop --preedit` flag (see *Streaming preedit (R9)* below). The
  **default remains commit-only** — without the flag no backend renders
  provisional text and FR-012 is fully in force.
- No post-processing beyond what the inference backend emits (out of scope).
- No Wayland-native (`input_method_v2`) backend here (kept addable, R3).

## Streaming preedit (R9) — landed 2026-07-27 (opt-in)

Implemented as `myna-desktop --preedit` + `DesktopController::builder().preedit(true)`:
the controller routes `OrchestratorEvent::Unstable` (feature 007's
`disposition: unstable` deltas — the streaming-era successor to the `Snippet`
this section originally named) → `set_preedit` (volatile, replaced each update)
and `Final`/`Done` → `commit` (clears preedit, inserts stable text) — but only
when the flag is on AND `supports_preedit()`. `IbusInjector` implements it via
`UpdatePreeditText` (single underline attribute spanning the hypothesis,
char-indexed) / `HidePreeditText`, with the same commit-time secure-purpose
re-check (I5/F2) applied to preedit. Wire detail (root-caused 2026-07-28):
the daemon parses the engine signal **strictly as `(vubu)`** — the `mode`
arg (`IBUS_ENGINE_PREEDIT_CLEAR`) is mandatory, and the attr list must be
variant-wrapped in the `IBusText` (`(sa{sv}sv)`); a 3-arg `(vub)` emission
or an inline attr list is dropped silently by the daemon. FR-012's commit-only guarantee is relaxed
*for the preedit region under the opt-in only*; every other invariant here
(target fixed at start, secure-field refusal, literal-text-only, idempotent
cancel/end, no committed-text retraction) is unchanged. Guarantees I2–I12
continue to hold, plus:

| # | Given | When | Then | Spec |
|---|-------|------|------|------|
| I13 | preedit enabled, preedit-capable backend | `Unstable` hypotheses arrive | each is rendered via `set_preedit` (replaced, never via `commit`); a pending stable burst is committed *before* the next preedit tail; `commit` clears the region | FR-012 (relaxed, opt-in), R9 |
| I14 | preedit enabled, focus lost mid-session | `Unstable`/`Final` tail arrives | neither preedit nor commit lands after focus loss | FR-014, SC-007 |

Default-off rationale: in-field hypothesis display is still design-contested
(UD136) — the flag exists to evaluate it live before any default flips.
Tests: `client/myna-desktop/tests/controller.rs` (I13/I14 hermetic),
`inject/ibus.rs` (preedit GVariant shape).
