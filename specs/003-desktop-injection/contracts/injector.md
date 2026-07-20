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
    /// FUTURE (streaming preedit, R9): render a volatile in-flight hypothesis in
    /// the target's preedit region, replaced on the next call and cleared by
    /// `commit`. Default no-op; only backends with a real preedit region honor
    /// it. NOT called in the commit-only MVP.
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

- No preedit/provisional rendering in the target — **commit-only MVP**. The seam
  is shaped for it (`set_preedit`/`supports_preedit`, R9) but the controller does
  not route `Snippet` to preedit and no backend enables it here. `set_preedit`
  stays a no-op default; `supports_preedit()` returns `false`.
- No post-processing beyond what the inference backend emits (out of scope).
- No Wayland-native (`input_method_v2`) backend here (kept addable, R3).

## Future extension: streaming preedit (R9)

When enabled in a later iteration: the controller routes
`OrchestratorEvent::Snippet` → `set_preedit` (volatile, replaced each update) and
`Final`/`Done` → `commit` (clears preedit, inserts stable text) — but only when
`supports_preedit()`. FR-012's commit-only guarantee is relaxed *for that
iteration*; every other invariant here (target fixed at start, secure-field
refusal, literal-text-only, idempotent cancel/end, no committed-text retraction)
is unchanged. Guarantees I2–I12 continue to hold; a new row would assert
"successive `set_preedit` calls replace, and `commit` clears the preedit region."
