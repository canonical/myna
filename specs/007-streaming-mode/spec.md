# Feature Specification: Dual-Mode Streaming Transcription

**Feature Branch**: `007-streaming-mode`

**Created**: 2026-07-27

**Status**: Draft

**Input**: Add a streaming transcription mode alongside the existing batch mode. Streaming shows committed chunks of text progressively as the user speaks. Gated on hardware tier support. The IE115 wire protocol must carry a committed/unstable discriminant so clients can distinguish inject-safe text from provisional hypotheses. Informed by interop experiments against the canonical/whisper-snap (Collabora WhisperLive) adapter and the UD136 design review.

## User Scenarios & Testing *(mandatory)*

### User Story 1 — User sees text appear while still speaking (Priority: P1)

A user on hardware that supports streaming (capable GPU or fast CPU tier) activates dictation via their configured hotkey, begins speaking, and sees committed chunks of their speech appear in the focused text field *while still speaking* — before they release the hotkey. Each chunk that appears is final (never retracted or rewritten); if the user stops and reads mid-utterance, what they see is correct so far. When they release the hotkey, any remaining text is committed and the session ends.

**Why this priority**: This is the headline user-visible feature — the reason streaming mode exists. Without progressive committed text, there is no streaming experience.

**Independent Test**: Activate dictation on a streaming-capable tier, speak a multi-sentence utterance; observe that text appears incrementally (at least 2 committed segments before hotkey release) and all committed text is accurate. Verify no committed text is ever retracted or overwritten.

**Acceptance Scenarios**:

1. **Given** a streaming-capable hardware tier and a focused text field, **When** the user activates dictation and speaks continuously for 8+ seconds, **Then** at least one committed text segment appears in the field before the hotkey is released.
2. **Given** committed text has appeared mid-utterance, **When** the user releases the hotkey, **Then** the final committed text is a superset of (starts with) the already-visible committed segments — nothing previously shown is retracted.
3. **Given** a streaming session produces committed segments, **When** comparing the final concatenated output to a reference transcript, **Then** accuracy (WER) is within the same tolerance as batch mode on the same model and hardware.

---

### User Story 2 — Batch mode remains the default on lower hardware tiers (Priority: P1)

A user on hardware that cannot sustain streaming (RTF ≥ ~1.0 for the active model — weak CPU, no GPU) activates dictation and gets the existing batch experience: text appears only after the hotkey is released and inference completes. There is no degraded or stuttering streaming experience; the system selects batch mode automatically based on the hardware tier.

**Why this priority**: Equal to P1 because shipping a broken streaming experience on incapable hardware would be worse than no streaming at all. The tier gate protects the majority of users who lack a GPU.

**Independent Test**: On a CPU-only machine where the active model's RTF exceeds the streaming threshold, activate dictation; verify text appears only after release, with no partial fragments. Confirm the mode was selected automatically (user did not configure it).

**Acceptance Scenarios**:

1. **Given** a hardware tier where the model's measured RTF ≥ 1.0, **When** the user activates dictation, **Then** the system operates in batch mode — no progressive text appears until the hotkey is released and inference completes.
2. **Given** an explicit user override to force streaming on a low tier, **When** the user activates dictation, **Then** the system either honors the override (with potential latency degradation the user accepted) or refuses with a clear explanation.

---

### User Story 3 — User can choose between streaming and batch in settings (Priority: P2)

A user on a capable tier who prefers the batch experience (text appears all at once after speaking) can switch to batch mode in settings. Conversely, a power user on a borderline tier can force streaming. The setting persists across sessions.

**Why this priority**: Configurability is a design-review requirement (UD136 review thread: "these behaviours will eventually need to be configurable, because reasonable minds can disagree"). It's P2 because the automatic tier-based default must work correctly first.

**Independent Test**: Change the mode setting, activate dictation, verify the selected mode is honored regardless of what the tier would have defaulted to.

**Acceptance Scenarios**:

1. **Given** a capable tier defaulting to streaming, **When** the user sets mode to "batch" in settings, **Then** subsequent dictation sessions use batch mode.
2. **Given** any tier, **When** the user sets mode to "streaming", **Then** the system attempts streaming (with the understanding that low tiers may degrade).
3. **Given** the user changes the setting, **When** they close and reopen the application, **Then** the setting persists.

---

### User Story 4 — The wire protocol distinguishes committed from unstable text (Priority: P1)

Backend servers (our own and third-party IE115 peers like the canonical/whisper-snap adapter) communicate transcription events where each piece of text is explicitly marked as either "committed" (safe to inject/display permanently) or "unstable" (provisional, may be revised or retracted). Clients use this discriminant to decide what to inject into the text field versus what to display as a transient hypothesis (if hypothesis display is ever enabled).

**Why this priority**: P1 because without an explicit wire discriminant, the client cannot safely inject streaming text — it must either inject everything (risking garbage from revisions, as observed in the interop experiments) or inject nothing (defeating streaming). This is the protocol foundation for P1/Story 1.

**Independent Test**: Connect a test client to a streaming-capable backend; send audio; verify every transcription event carries an explicit committed/unstable marker. Verify that committed text is never followed by a revision of that same text. Verify that unstable text may be revised or superseded.

**Acceptance Scenarios**:

1. **Given** a streaming session over the IE115 wire, **When** a `transcription.delta` event arrives, **Then** it carries an explicit field indicating whether the text is committed (append-only, never retracted) or unstable (provisional, may be revised).
2. **Given** text marked as "committed" has been received, **When** subsequent events arrive, **Then** no event retracts, replaces, or contradicts the committed text.
3. **Given** text marked as "unstable" has been received, **When** a revision event arrives, **Then** it clearly identifies what it supersedes (by utterance ID or offset) so the client can replace the displayed hypothesis.

---

### User Story 5 — Interop findings are fed back to the canonical/whisper-snap team (Priority: P2)

The protocol decisions and interop gaps discovered during experiments against the canonical/whisper-snap adapter are documented and communicated to the colleagues responsible for that snap, so both implementations can converge on a shared IE115 streaming wire shape.

**Why this priority**: P2 because it's a collaboration deliverable, not a user-facing feature — but it gates the interop story (their snap being a test fixture requires protocol alignment).

**Independent Test**: A written interop report exists, has been shared with the canonical/whisper-snap team, and covers: the 6 identified protocol gaps, the proposed committed/unstable discriminant, and the session.update/reload behavior recommendation.

**Acceptance Scenarios**:

1. **Given** the interop experiments completed (T63), **When** the streaming spec is ratified, **Then** an interop report documenting protocol gaps and proposed resolutions is delivered to the canonical/whisper-snap team.
2. **Given** the report is delivered, **When** the colleagues review it, **Then** the outcomes (accepted/rejected/counter-proposed) are recorded in the spec.

---

### User Story 6 — Streaming adapters emit progressive committed segments (Priority: P1)

The server-side adapters (Whisper via chunked re-decode, Nemotron via native transducer) emit committed text segments progressively during inference — not only at end-of-utterance. Each adapter's streaming granularity matches its architecture: Nemotron emits per-frame committed tokens (native streaming); Whisper emits coarser committed segments when the LocalAgreement algorithm stabilizes a chunk (bolt-on streaming with higher latency per commit).

**Why this priority**: P1 because without server-side streaming emission, the wire discriminant (Story 4) and client display (Story 1) have nothing to consume.

**Independent Test**: Send a 10+ second audio stream to each streaming adapter; verify that committed text events arrive before end-of-audio; measure time-to-first-committed and total committed segments per utterance.

**Acceptance Scenarios**:

1. **Given** audio streaming to a Nemotron adapter in streaming mode, **When** the audio contains recognizable speech, **Then** the first committed text event arrives within 2 seconds of audio start.
2. **Given** audio streaming to a Whisper adapter in streaming mode (capable tier), **When** the audio contains recognizable speech, **Then** at least one committed text event arrives before the audio stream ends (for clips ≥ 5 seconds).
3. **Given** any streaming adapter, **When** the full utterance completes, **Then** the concatenation of all committed segments equals the final transcript (no gaps, no overlaps).

---

### Edge Cases

- What happens when hardware tier changes mid-session (e.g., GPU throttles due to thermal)? → Mode does not change mid-session; tier is assessed at session start.
- What happens when a streaming adapter falls behind realtime (RTF crosses 1.0 during a session)? → The session continues in streaming mode but committed segments arrive with increasing delay; no audio is lost (the backend buffers). The next session re-assesses the tier.
- What happens when a client connects to a batch-only backend but requests streaming? → The backend operates in batch mode (single committed segment at end); the client falls back gracefully — this is indistinguishable from "streaming with one large segment."
- What happens when the connection drops mid-stream after committed text was injected? → The committed text stays (it was already injected and is append-only). The session terminates with an error; no further text appears. No phantom "done" is synthesised.
- What happens when revision (unstable text) arrives but hypothesis display is disabled? → The unstable events are silently discarded by the client; only committed text is injected.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST support two transcription modes: **streaming** (committed text segments arrive progressively during speech) and **batch** (all text arrives after end-of-utterance).
- **FR-002**: The system MUST automatically select streaming or batch mode based on the hardware tier's measured real-time factor (RTF) for the active model, with streaming enabled only when RTF < 1.0 at the assessed tier.
- **FR-003**: The user MUST be able to override the automatic mode selection via a persistent setting (streaming / batch / auto).
- **FR-004**: The IE115 wire protocol MUST carry an explicit committed/unstable discriminant on every transcription text event, so clients can distinguish inject-safe text from provisional hypotheses without relying on heuristics or payload emptiness.
- **FR-005**: Committed text on the wire MUST be append-only: once a committed segment is emitted, no subsequent event may retract, revise, or contradict it. The server guarantees this invariant.
- **FR-006**: Unstable (provisional) text on the wire MUST identify what it supersedes, so a client can replace a displayed hypothesis without accumulating stale text.
- **FR-007**: The system MUST NOT inject unstable text into the focused text field by default. Only committed text is injected. Hypothesis display (e.g., greyed-out unstable text with differentiating formatting) is a separate, opt-in feature gated on design sign-off.
- **FR-008**: Each streaming-capable adapter MUST emit at least one committed segment before end-of-audio for any utterance longer than 5 seconds on its target tier.
- **FR-009**: The final concatenation of all committed segments for an utterance MUST equal the full final transcript — no gaps, no overlaps, no duplicated text.
- **FR-010**: The batch mode path MUST remain unchanged from today's behavior: a single committed segment (the full transcript) delivered after inference completes. Batch mode is a degenerate case of streaming (one segment).
- **FR-011**: The mode in effect for a session MUST be communicated to the client before audio flows, so the client can configure its display (e.g., show a "streaming" indicator vs "processing" indicator).
- **FR-012**: The RTF assessment for tier gating MUST be measurable via the existing testbed (`dev/matrix.py`) and recordable as a per-model, per-hardware baseline.
- **FR-013**: An interop report documenting the 6 protocol gaps discovered against the canonical/whisper-snap adapter MUST be produced and delivered to the colleagues responsible for that snap. The report MUST include: (a) the proposed committed/unstable discriminant, (b) the session.update reload behavior, (c) the empty-completed-as-reset anti-pattern, (d) the model.loaded/unloaded → STATUS alignment proposal, (e) the endpoint-path standardization question, (f) the binary-frame-support recommendation.
- **FR-014**: The `myna-dictate` testbed client MUST be able to operate in streaming-display mode: showing committed text as `»` lines progressively, and optionally showing unstable text as `~` lines (when hypothesis display is enabled via a flag).

### Key Entities

- **TranscriptionSegment**: A piece of transcribed text with a committed/unstable disposition, an utterance ID, a sequence offset within the utterance, and the text content.
- **StreamingMode**: An enum (Streaming | Batch | Auto) persisted as a user setting; Auto resolves to Streaming or Batch based on the tier assessment.
- **TierAssessment**: A per-model, per-hardware measurement of RTF that determines whether streaming is viable. Assessed at session start (or periodically in background).
- **InteropReport**: A document artifact covering protocol gaps, proposed resolutions, and status of colleague feedback.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: On a streaming-capable tier (RTF < 0.5), users see the first committed text within 3 seconds of beginning to speak (time-to-first-committed ≤ 3 s for Nemotron; ≤ 5 s for Whisper/AED on GPU).
- **SC-002**: Streaming mode accuracy (WER) on the real corpus is within 2 percentage points of batch mode accuracy for the same model — streaming does not meaningfully degrade quality.
- **SC-003**: On a batch-only tier (CPU-only, RTF > 1.0), users experience zero regression: behavior is identical to today's batch mode, with no partial fragments or UI changes.
- **SC-004**: The committed-text invariant (append-only, never retracted) holds across 100% of streaming sessions in automated test sweeps.
- **SC-005**: The interop report is delivered to the canonical/whisper-snap team within 2 weeks of spec ratification.
- **SC-006**: A test client can successfully stream against both our own streaming server and the canonical/whisper-snap adapter using the same protocol (IE115 with the committed/unstable discriminant), after protocol alignment is complete.
- **SC-007**: The mode-selection setting is discoverable and changeable by a user without documentation (settings UI or a single CLI command for the testbed).

## Assumptions

- The existing RTF measurements from `dev/matrix.py` are reliable enough to gate streaming. No new benchmarking infrastructure is required; the threshold value (~1.0) may be tuned from real-world data.
- Nemotron's native transducer architecture produces a monotonic committed frontier by design — no additional research is needed to make it streaming-safe. The adapter work is plumbing, not algorithm research.
- Whisper streaming (chunked re-decode / LocalAgreement) is known-viable from upstream (WhisperLive, whisper_streaming); the engineering challenge is integrating it into our adapter with the committed-only contract (coarser segments, higher latency per commit, but never retracted).
- The UD136 design review's contested "uncommitted hypothesis in-field with differentiating formatting" is deferred: this spec delivers the wire and server infrastructure; hypothesis display is a follow-up feature gated on design sign-off (FR-007).
- The canonical/whisper-snap team is receptive to protocol alignment feedback — they named their project "Myna Adapter" and target our wire.
- The `com.canonical.Myna.Dictation` D-Bus publisher (used by the GNOME extension) will need a streaming-state signal; this is a follow-up integration task, not a blocker for the core streaming work.
- Audio format negotiation (T33) and error taxonomy (T31) are independent work items; this spec assumes they proceed in parallel and does not block on them.
