# Feature Specification: Native PipeWire Capture Backend

**Feature Branch**: `002-native-pipewire-backend`

**Created**: 2026-07-15

**Status**: Draft

**Input**: User description: "Native pipewire-rs capture backend behind the existing CaptureBackend trait in rust/myna-audio. Adds device enumeration, channel pick/downmix on multi-channel interfaces, node selection by stable node.name, and graph-side resampling to the negotiated audio format. No subprocess — uses pipewire-rs directly. Reuses the existing CaptureSource, Ring, AudioStats, and ScriptedBackend test fixture. The PwRecordBackend subprocess backend may be retired or kept as fallback — decide during planning."

## Clarifications

### Session 2026-07-15

- Q: Retire the subprocess backend, or keep it as a fallback? → A: Retire it — the native backend is the sole live-capture backend after this feature.
- Q: Is device enumeration a point-in-time listing or a live API? → A: Live — enumeration reflects devices appearing/disappearing while running, with change notifications.

### Implementation finding 2026-07-15 (absent-target behavior)

- A **bogus / absent** `target` node.name does **not** produce a clear fault under the default WirePlumber session-manager policy: the graph falls back to the default source and captures. This is platform behavior, not a backend defect — `pw-record --target <bogus>` does the same (verified: captures from the default mic). So FR-004's "absent target → device-unavailable fault" (US2 scenario 3) is **downgraded to a known limitation**: strict absent-target faulting would need a session-manager policy/route change outside this crate. The load-bearing, testable guarantees remain: (a) a **resolvable** target captures *that* node (US2 scenario 1), and (b) a chosen target that later vanishes mid-capture faults via the stream error path (`DONT_RECONNECT` is set when a target is given). The `myna-cli --list-devices` output (US4) lets a user pick a real name, which is the intended path.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Dictate through the native backend, no subprocess (Priority: P1)

A person dictating on their Ubuntu desktop presses the push-to-talk hotkey, speaks, and
releases. Their words are captured from the default microphone and transcribed — exactly as
they are today through the subprocess backend, but now the capture path runs inside the
dictation process with no external `pw-record` command spawned.

**Why this priority**: This is the reason the feature exists. Removing the subprocess is the
core deliverable; everything else (device/channel selection, enumeration) builds on a working
in-process capture path. If only this ships, the product has a self-contained capture path
with one fewer moving part, no fork/exec on the hot path, and direct visibility into capture
health — a complete, shippable increment.

**Independent Test**: Run the existing dictation client against a running inference backend
using the native backend, capturing from a virtual audio interface fed a known utterance;
assert the transcript matches, capture starts at hotkey press, and no child process is
spawned.

**Acceptance Scenarios**:

1. **Given** a system with a working default input device, **When** the user presses the
   hotkey and speaks, **Then** audio is captured into the ring beginning at press and the
   spoken utterance is transcribed correctly on release.
2. **Given** the device delivers audio in a format other than the negotiated format (a
   different sample rate or channel count), **When** capture runs, **Then** the audio reaching
   the consumer is in exactly the negotiated format, converted graph-side.
3. **Given** an active capture session, **When** the user releases the hotkey (graceful stop),
   **Then** already-captured audio drains cleanly and the stream ends without a fault.
4. **Given** an active capture session, **When** the default device disappears mid-capture,
   **Then** the session ends with a single, descriptive capture fault (not a silent stall or an
   empty transcript).
5. **Given** no capture is running, **When** the capture path operates, **Then** no external
   command is spawned and no audio is written to disk.

---

### User Story 2 - Choose a specific input device that stays chosen (Priority: P2)

A person with more than one microphone (a laptop mic plus a USB headset, say) selects which
device dictation listens to, and that choice keeps working across reboots, device
reconnections, and audio-graph changes.

**Why this priority**: Multi-device setups are common. Without stable selection, dictation
either always uses the system default (wrong for many users) or breaks when the audio graph
renumbers devices. Depends on US1's working capture path.

**Independent Test**: With two named virtual input nodes present, construct the source
targeting one by its stable name, feed distinct signals to each, and assert the captured audio
came from the targeted node; then simulate a graph change that renumbers nodes and assert the
same named target still resolves to the same device.

**Acceptance Scenarios**:

1. **Given** multiple input devices, **When** the user targets a device by its stable name,
   **Then** audio is captured from that device and not the system default.
2. **Given** a device was targeted by stable name, **When** the audio graph changes such that
   volatile identifiers are reassigned, **Then** the same target still selects the same
   physical device.
3. **Given** a targeted device is absent when capture begins, **When** the user presses the
   hotkey, **Then** ideally the session ends with a clear "device unavailable" fault naming the
   target. **Known limitation (see Implementation finding 2026-07-15):** under the default
   WirePlumber policy the graph instead falls back to the default source (as `pw-record` does),
   so this fault is not guaranteed; the enforced guarantee is that a *resolvable* target selects
   that node (scenario 1) and a chosen device that vanishes mid-capture faults.

---

### User Story 3 - Capture the right channels on a multi-channel interface (Priority: P3)

A person using a professional audio interface whose microphone is wired to channels 9 and 10
(not the default 1/2) tells dictation which channels to listen to, and only those channels are
captured (downmixed to the negotiated channel count).

**Why this priority**: A real limitation of the subprocess backend (it rejects channel
selection outright). Valuable to pro-audio users but a minority; it builds on US1 and US2.

**Independent Test**: With a multi-channel virtual device carrying a signal only on specified
channels, construct the source selecting those channel indices, and assert the captured audio
contains the signal (and that omitted channels are excluded).

**Acceptance Scenarios**:

1. **Given** a multi-channel input device, **When** the user selects specific channel indices,
   **Then** only those channels are captured and combined into the negotiated channel layout.
2. **Given** selected channel indices that the device does not have, **When** capture begins,
   **Then** the session ends with a clear fault rather than silently capturing wrong or empty
   channels.

---

### User Story 4 - See the available input devices, live (Priority: P3)

A person (or the settings UI acting for them) asks "what can I dictate from?" and gets the list
of available input devices with their stable names and human-readable labels, so they can make
the choice US2 acts on — and when a device is plugged in or unplugged while the list is open,
the list updates to reflect it without being re-requested.

**Why this priority**: Enumeration is the discovery step that makes stable-name selection (US2)
usable by a human — you need to see the names to pick one. Live updates keep a settings chooser
honest when a headset is plugged in or removed mid-session. It is a distinct, separately testable
capability and is lower priority because selection can be exercised with known names before a
listing UI exists.

**Independent Test**: With a known set of virtual input nodes present, request the device list
and assert each expected device appears with its stable name and a display label; then add and
remove a node and assert the observer is notified of the appearance and disappearance.

**Acceptance Scenarios**:

1. **Given** a set of input devices, **When** the device list is requested, **Then** each
   available input device is returned with its stable name and a human-readable label.
2. **Given** no input devices are present, **When** the device list is requested, **Then** an
   empty list is returned (not an error).
3. **Given** an active device listing/observer, **When** an input device appears or disappears,
   **Then** the observer is notified of the change (the appearing device with its stable name and
   label; the disappearing device by its stable name) without re-requesting the full list.

---

### Edge Cases

- **Device vanishes at start vs mid-capture**: absent-at-start → "device unavailable" fault
  naming the target; disappears mid-capture → a single descriptive fault, matching the existing
  contract that a fault is one `Err` then end, never an empty stream masquerading as a clean end.
- **Consumer stalls past the ring depth** (a pathological cold load): oldest audio ages out
  (drop-oldest) and the dropped duration is surfaced on the stats tap — capture is never blocked
  and the session is never failed for overflow. (Unchanged from the existing ring contract.)
- **Graph renumbering during a session**: a selection made by stable name must not be
  invalidated by volatile-identifier churn.
- **Abrupt cancel (drop the stream)**: capture stops, the ring is discarded, nothing more is
  delivered, resources are released promptly.
- **Requested channel indices out of range**: rejected with a clear fault, never silently
  mis-captured.
- **Device appears/disappears while a chooser is open**: the live listing notifies the observer
  of the change rather than going stale; a disappearing device that was the current target for a
  not-yet-started session surfaces as "device unavailable" at press (per US2).
- **Device native format differs from negotiated** (rate and/or channels): converted graph-side
  so the consumer always receives exactly the negotiated format.
- **Underrun / device xrun**: the capture stack already presents a continuous timeline (gaps
  padded upstream); no silence-fill or underrun concept is added here.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST provide a capture backend that captures live microphone audio
  directly through the platform audio stack, without spawning an external process.
- **FR-002**: The new backend MUST plug into the current capture-backend seam with no change to
  the consumer-facing capture interface, the adapter core, the bounded ring, or the stats tap;
  the scripted fake fixture continues to occupy that same seam for hermetic tests.
- **FR-003**: The backend MUST produce audio in exactly the negotiated format, performing any
  required sample-rate conversion and channel downmix within the audio graph (the consumer and
  inference backend never resample).
- **FR-004**: The backend MUST support selecting a specific input device by a stable identifier
  that survives audio-graph changes and device reconnection.
- **FR-005**: When no specific device is selected, the backend MUST capture from the system
  default input device.
- **FR-006**: The backend MUST support selecting specific channel indices on multi-channel input
  devices and combining them into the negotiated channel layout.
- **FR-007**: The backend MUST reject a channel selection the device cannot satisfy with a clear
  fault, rather than capturing wrong or empty channels.
- **FR-008**: The system MUST provide a way to enumerate available input devices, returning each
  device's stable identifier and a human-readable label.
- **FR-008a**: Device enumeration MUST be live: an active observer MUST be notified when an input
  device appears or disappears while the system is running, without re-requesting the full list.
  Appearance notifications carry the device's stable identifier and label; disappearance
  notifications carry at least the stable identifier.
- **FR-009**: Capture MUST begin at hotkey press (filling the bounded ring) with the push to the
  inference backend gated on model readiness, preserving the existing pre-ready buffering
  behavior.
- **FR-010**: A failure to open the device MUST surface as a capture error at start; a failure
  during capture MUST surface as exactly one terminal fault on the stream, then end — never an
  empty stream presented as a clean end.
- **FR-011**: A graceful stop MUST drain already-captured audio and then end the stream cleanly;
  an abort (dropping the stream) MUST stop capture, discard the ring, and release resources.
- **FR-012**: The stop signal MUST be observed within the existing promptness bound (~250 ms).
- **FR-013**: Captured audio MUST NOT be persisted to disk; it MUST live only in the bounded
  in-memory ring and be discarded when the session ends.
- **FR-014**: The stats tap MUST continue to report capture health (levels, clipping, captured
  and dropped durations) updated at capture time, with no raw audio samples exposed.
- **FR-015**: The existing scripted test fixture MUST remain the fake backend for hermetic tests;
  the native backend's own tests MUST be runnable against a virtual audio interface without
  physical hardware, and against real hardware without code changes.
- **FR-016**: The existing subprocess backend MUST be removed; after this feature the native
  backend is the sole live-capture backend. (The scripted fake fixture remains, for tests.)

### Key Entities *(include if feature involves data)*

- **Input device descriptor**: a discoverable input device — its stable identifier (used for
  selection) and a human-readable label (shown to a person choosing).
- **Device change notification**: a live signal that an input device appeared or disappeared —
  identifying the device by its stable identifier (and, on appearance, its label).
- **Capture specification**: the existing request describing what to capture — target device
  (optional stable identifier), channel selection (optional indices), negotiated format, and the
  stop signal.
- **Capture stats snapshot**: the existing capture-health reading — levels, clipping flag,
  captured duration, dropped duration — carrying no audio content.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A spoken utterance captured through the native backend from a virtual audio
  interface transcribes to its known reference text (no accuracy regression from the capture
  change), matching the transcript the subprocess backend produced for the same utterance before
  its removal.
- **SC-002**: A full dictation session completes with no external process spawned and no audio
  file written anywhere on disk.
- **SC-003**: A device selected by stable identifier is still the device captured after an
  audio-graph change that reassigns volatile identifiers, in 100% of trials.
- **SC-004**: On a multi-channel device, a signal present only on the selected channels is
  captured and a signal on unselected channels is excluded.
- **SC-005**: Enumeration lists every input device present in a known virtual-audio setup, each
  with a stable identifier and a display label.
- **SC-006**: Under a healthy session (drain keeps up), the dropped-audio duration reported by
  the stats tap is zero.
- **SC-007**: A graceful stop drains all captured audio and ends cleanly; a device failure ends
  the session with exactly one descriptive fault — verified for both start-time and mid-capture
  failures.
- **SC-008**: End-to-end capture latency and per-session resource usage (peak/steady memory, CPU)
  through the native backend are within the recorded watermark baselines and declared tolerances
  for the capture path on the reference environments.
- **SC-009**: The stop signal is honored within 250 ms in all stop/abort scenarios.

## Assumptions

- This is a shipped Rust system component; the project constitution applies in full (test-first
  for the new backend, integration-test readiness on both a virtual-audio VM and real hardware,
  performance watermark baselines, and the privacy/offline invariants).
- The consumer-facing capture contract, the adapter core (`CaptureSource`), the bounded ring, the
  stats tap (`AudioStats`), and the scripted fake fixture are reused unchanged; this feature adds
  a new native backend behind the existing seam plus a live device-enumeration capability, and
  removes the subprocess backend.
- The negotiated audio format is chosen by the session controller from the inference backend's
  advertised capabilities and passed in at construction; the backend never chooses the format.
- Audio-sample encoding remains the current single wire encoding (16 kHz mono S16LE by default);
  the encoding-discriminant question is a separate, out-of-scope team discussion.
- Device enumeration is live: it lists current devices and notifies an observer as devices appear
  and disappear, so a chooser stays current without polling (FR-008/FR-008a).
- Real digital-signal processing (noise suppression, echo cancellation, etc.) stays in the audio
  graph upstream of the capture node and is out of scope here; the backend performs only
  selection, downmix, and format conversion via the graph, plus observation via the stats tap.
- Voice-activity detection stays out of scope (push-to-talk: the hotkey is the trigger).
- The subprocess backend is retired as part of this feature (clarified 2026-07-15); the native
  backend becomes the sole live-capture path, with the scripted fake fixture retained for tests.
