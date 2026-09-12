# Quickstart: Desktop Session Controller + Text Injection

**Feature**: 003-desktop-injection

Runnable validation that proves the T21/T22 last-mile end-to-end. Assumes the
workspace builds (`cd client && cargo build`), a running `myna-server` (or
inference snap) for the live path, a Wayland/GNOME session, a running IBus
daemon, and an `xdg-desktop-portal` with a GlobalShortcuts backend.

## Prerequisites

- Ubuntu Desktop, Wayland, GNOME; IBus running (`ibus address` responds).
- `xdg-desktop-portal` + GlobalShortcuts backend (GNOME 50 verified).
- `libgtk-4-dev` present (declared in the Workshop definition — see the
  foundational task): `pkg-config --modversion gtk4`.
- Rust workspace toolchain (`rust-version` 1.75+).
- An inference backend, e.g.
  `uv run myna-server --adapter whisper --model base --socket /tmp/ubustt.sock`.

## 1. Hermetic suite stays green (no IBus/portal/GTK/display)

The controller policy + boundary mappings must pass with all real backends
mocked and the `ui-gtk` feature off.

```shell
cd client
cargo test -p myna-desktop --no-default-features   # mocks only; no GTK/DBus/display
cargo test --workspace                              # nothing else regresses
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: session-lifecycle transitions, autorepeat dedup, focus-change→end,
secure-field refusal, commit-only routing, and the `OrchestratorEvent →
IndicatorState` mapping all pass; no network, no D-Bus, no display.

## 2. Integration suites against real IBus + portal (VM/hardware)

Env-gated; identical code on the desktop VM and on hardware (Principle II).

```shell
# real IBus commit / focus-out / secure-field (needs a running IBus daemon):
MYNA_IBUS_TESTS=1 cargo test -p myna-desktop --test ibus_hw

# real GlobalShortcuts bind / Activated+Deactivated (needs the portal):
MYNA_PORTAL_TESTS=1 cargo test -p myna-desktop --test portal_hw
```

Expected outcomes (map to contract rows):
- IBus: commit lands in a focused test entry (I1); password field refused (I5);
  focus-out and target-gone emit events (I8, I9); global-engine restored (I11).
- Portal: a bound test shortcut yields `Press` on activate and `Release` on
  deactivate (T1, T2); autorepeat collapses to one `Press` (T3).

## 3. Live push-to-talk dictation into the focused app (the headline)

```shell
uv run myna-server --adapter whisper --model base --socket /tmp/ubustt.sock &
cd client && cargo build --release

# the shipped desktop app: binds the global shortcut, shows the indicator,
# injects via IBus into the app focused at press.
./target/release/myna-desktop --socket /tmp/ubustt.sock --language en
```

Then: focus a text editor → press & hold the bound shortcut (confirm/rebind via
GNOME's shortcut dialog on first run) → speak a known utterance → release.

Expected: the indicator appears on press (recording→transcribing→finalizing), the
committed transcript is inserted into the editor and matches, nothing is typed
elsewhere, and no audio file is written (SC-001, SC-002, SC-009, US1/US2/US3).
(SC-009 no-persist is inherited from feature 002's capture path; see tasks T021.)

## 4. Safety checks

```shell
# focus-change safety: start dictation in editor A, alt-tab to B while held.
# secure-field: focus a password field, try to start.
```

Expected: switching focus mid-session inserts **zero** characters into B and ends
the session safely (SC-007); the password field refuses to start with a clear
notification (SC-008); closing the target window mid-session cancels safely.

## 5. Performance watermark check (Principle III)

```shell
# activation→indicator, press→capture, per-segment commit, teardown latencies:
cargo test -p myna-desktop perf_ -- --nocapture
```

Expected: indicator visible within 100–200 ms of activation (SC-005); press→
capture < 100 ms; commit adds < 50 ms/segment; within declared tolerance of the
checked-in baselines; no capture-path regression versus feature 002.

## 6. Legacy cleanup verification

```shell
# the Python desktop stubs are gone and nothing depends on them:
cd server && uv run pytest -q            # green after removal (SC-010)
uv run python -c "import myna; import myna.core; import myna.server"  # imports OK
test ! -e src/myna/desktop && echo "myna.desktop removed"
```

## Done / acceptance

- [ ] Hermetic + workspace suites green, `ui-gtk` off (step 1)
- [ ] IBus + portal integration suites green on VM and hardware (step 2)
- [ ] Live push-to-talk dictation injects the correct transcript into the focused
      app with a visible indicator and no subprocess/disk write (step 3)
- [ ] Focus-change and secure-field safety hold (step 4)
- [ ] Latency watermarks within tolerance (step 5)
- [ ] `server/src/myna/desktop/` removed; Python suite + imports green (step 6)
