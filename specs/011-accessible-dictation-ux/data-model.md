# Phase 1 Data Model: Accessible Dictation UX

Entities per spec Key Entities, made concrete for implementation.

## `DictationState` (User-visible state)

The set already exists as `controller::DictationState`/`IndicatorState` variants
(feature 003/004: `Idle`/`Hidden`, `Recording`/listening, `Transcribing`,
`Finalizing`, `Error{recoverable, message}`) plus this feature's addition of an
explicit `notice` case for recoverable completions (already partially present
via `IndicatorState::recoverable`). Each state carries:

| Field | Type | Notes |
|---|---|---|
| `id` | enum tag (`idle`, `loading`, `recording`, `transcribing`, `finalizing`, `notice`, `error`) | Stable wire/JSON identifier shared with `coverage-matrix.json` and `states.js` — matches the existing `org.myna.Dictation` wire vocabulary (`states.js`'s `DictationState`), not a new naming scheme |
| `accessible_name` | `String` | Short label, e.g. "Dictation: listening" — content-free (constitution V, FR-003) |
| `accessible_description` | `String` | One sentence, content-free, e.g. "Recording your speech" |
| `severity` | `Option<Severity>` (`Recoverable`, `Critical`) | Only set for `notice`/`error`; drives FR-025 persistence |
| `announcement_text` | `String` | What is actually spoken; may be shorter than description (SC-003 latency budget) |

State transitions are validated the same way `controller.rs` already validates
`DictationState` transitions (no new transition legality rules introduced by
this feature).

## `FeedbackChannel`

| Value | Visual? | Notes |
|---|---|---|
| `shell_hud` | yes | `myna-shell` bottom-center pill |
| `gtk_overlay` | yes | opt-in `GtkIndicator` |
| `notification` | yes | `notify-rust` toast |
| `sound_cue` | no (audible, not visual) | optional, per-cue disableable |
| `atspi_announcement` | no | speech + braille via the same AT-SPI path (R1) |
| `programmatic_query` | no | accessible name/description/state, queryable on demand (FR-001) |
| `terminal_text` | yes (screen-reader-readable) | `myna-cli` stdout/stderr |

Each channel is independently disableable per FR-010; `programmatic_query` and
`atspi_announcement` are never disableable below "queryable" (FR-004's
verbosity floor only silences *proactive* announcements, never the query).

## `coverage-matrix.json` (checked-in, shared by Rust + GJS)

```json
{
  "states": [
    {
      "id": "idle",
      "channels": {"visual": ["shell_hud"], "non_visual": ["programmatic_query"]},
      "colour_only": false,
      "sound_only": false
    },
    {
      "id": "loading",
      "channels": {"visual": ["shell_hud"], "non_visual": ["atspi_announcement", "programmatic_query"]},
      "colour_only": false,
      "sound_only": false
    },
    {
      "id": "recording",
      "channels": {"visual": ["shell_hud", "gtk_overlay"], "non_visual": ["atspi_announcement", "sound_cue", "programmatic_query"]},
      "colour_only": false,
      "sound_only": false
    },
    {
      "id": "transcribing",
      "channels": {"visual": ["shell_hud", "gtk_overlay"], "non_visual": ["atspi_announcement", "programmatic_query"]},
      "colour_only": false,
      "sound_only": false
    },
    {
      "id": "finalizing",
      "channels": {"visual": ["shell_hud", "gtk_overlay"], "non_visual": ["atspi_announcement", "programmatic_query"]},
      "colour_only": false,
      "sound_only": false
    },
    {
      "id": "notice",
      "channels": {"visual": ["shell_hud", "gtk_overlay", "notification"], "non_visual": ["atspi_announcement", "programmatic_query"]},
      "colour_only": false,
      "sound_only": false
    },
    {
      "id": "error",
      "channels": {"visual": ["shell_hud", "gtk_overlay", "notification", "terminal_text"], "non_visual": ["atspi_announcement", "sound_cue", "programmatic_query"]},
      "colour_only": false,
      "sound_only": false
    }
  ]
}
```

**Invariant** (enforced by `coverage.rs` and `coverage.test.js`): every entry's
`channels.visual` and `channels.non_visual` are both non-empty, and
`colour_only`/`sound_only` are both `false` for every entry — the gate fails
the build the moment either goes empty/true (SC-002).

## `AccessibilityPreferenceSet`

Read-only from this feature's perspective (owned/persisted by the separate
settings feature, spec Assumptions):

| Field | Type | Default (FR-004) |
|---|---|---|
| `verbosity` | `Off \| FailuresOnly \| AllTransitions` | `AllTransitions` |
| `sound_cues_enabled` | `bool` (+ per-cue override) | ship enabled (spec Assumptions) |
| `silence_auto_stop_seconds` | `Option<u32>` | existing T59 default |

Every consuming process (`myna-desktop`, `myna-shell`, `myna-cli`) reads the
same GSettings/dconf keys directly — no new IPC introduced by this feature
(project-plan T54 dependency, named not solved).

## `FailurePresentation`

```rust
struct FailurePresentation {
    id: &'static str,          // stable key, e.g. "no_microphone"
    message: String,           // plain language, no error codes/jargon (FR-023)
    recovery_action: String,   // "what to do next"
    severity: Severity,        // Recoverable | Critical (FR-025)
}
```

One instance per known ad-hoc failure string in `controller.rs`/`inject::InjectError`
(R5); rendered identically by indicator, notification, terminal, and
announcement (FR-024).
