# Diagrams: Native PipeWire Capture Backend

**Feature**: `002-native-pipewire-backend` · **Crate**: `rust/myna-audio`

Visual companion to [spec.md](spec.md), [data-model.md](data-model.md), and the
contracts ([capture-backend.md](contracts/capture-backend.md) guarantees C1–C14,
[device-enumeration.md](contracts/device-enumeration.md) guarantees E1–E7). These
document the **shipped** design (native `pipewire-rs`, graph-side conversion, no
in-crate DSP) — distinct from the earlier `001-rust-audio-adapter` sketch
(PulseAudio fallback, in-process resample/VAD). Cross-ref the runtime docs in
`docs/audio-adapter-api.md` (§5 backends, §7 conversion, §9 channels).

## 1. Architectural block diagram

`PipeWireBackend` drops in behind the **unchanged** `CaptureBackend` seam; the
adapter core (`CaptureSource` = ring + stats tap + re-chunker) and the consumer
types are untouched (FR-002). `InputDevices` is an independent sibling entry
point sharing only the PipeWire connection model. The RT side is the PipeWire
loop thread's `process` callback; the consumer side is whatever thread drains
`CaptureStream` (in production, the `myna-orchestrator` FSM via `myna-dictate`).

```mermaid
flowchart LR
    subgraph consumer["Consumer (rust/): myna-dictate → myna-orchestrator FSM"]
        FSM["Session/residency FSM<br/>drains after 'ready'"]
        CHOOSER["--list-devices / chooser"]
    end

    subgraph crate["myna-audio"]
        SRC["CaptureSource (AudioSource)<br/>re-chunk → ~100 ms whole frames"]
        STATS["stats tap<br/>watch&lt;AudioStats&gt; (RMS/peak/clip,<br/>captured/dropped)"]
        RING["bounded ring (drop-oldest, §6)<br/>fills at press; drained after ready"]
        STREAM["CaptureStream (drop-guard:<br/>drop ⇒ stop + discard ring)"]

        subgraph pwbe["PipeWireBackend (native)"]
            LOOP["dedicated PipeWire loop thread<br/>≤100 ms stop-poll timer (C11)"]
            CB["process callback (RT)<br/>width guard → channel select/downmix<br/>→ Producer::push (never blocks)"]
        end

        DEV["InputDevices<br/>list() + watch&lt;Vec&lt;InputDevice&gt;&gt;"]
    end

    subgraph graph["PipeWire graph (server)"]
        STREAMNODE["capture Stream<br/>SPA audio-raw = negotiated S16LE"]
        CONV["graph resample/downmix (§7, FR-003)"]
        NODES["input nodes (mics, virtual)<br/>media.class = Audio/Source"]
        REG["registry (globals)"]
    end

    FSM -->|"capture(CaptureSpec)"| SRC
    SRC --> pwbe
    LOOP --> CB
    CB -->|"push(bytes)"| RING
    CB -. levels/counters .-> STATS
    RING --> STREAM
    STREAM -->|"PcmChunk: exactly spec.format"| FSM
    CB <--> STREAMNODE
    STREAMNODE --- CONV
    CONV --- NODES
    CHOOSER -->|"node.name"| DEV
    DEV <-->|"watch globals"| REG
    REG --- NODES
    DEV -. "node.name → CaptureSpec.target" .-> FSM
```

## 2. Sequence: push-to-talk session — capture at press, drain after ready (C1, C8, FR-009)

The ring fills from the moment of press; the *push* to the consumer is what waits
on the model being `ready`, so nothing said during a cold load is lost (up to
ring depth). Graceful stop drains, then ends with no `Err`.

```mermaid
sequenceDiagram
    participant U as User
    participant FSM as Orchestrator FSM
    participant SRC as CaptureSource
    participant BE as PipeWireBackend (loop thread)
    participant G as PipeWire graph
    participant INF as myna-server

    U->>FSM: hotkey press
    FSM->>SRC: capture(CaptureSpec{format,target,channels,stop})
    SRC->>BE: start(spec, producer)
    BE->>G: connect input Stream (SPA = negotiated S16LE)
    G-->>BE: process callbacks (RT)
    loop every quantum
        BE->>SRC: push(bytes) — ring fills at press
    end
    Note over FSM,INF: model still loading → FSM defers draining
    INF-->>FSM: transcription.progress phase=ready
    loop while held
        FSM->>SRC: drain CaptureStream
        SRC-->>FSM: PcmChunk (exactly spec.format)
        FSM->>INF: PCM frames (streaming)
    end
    U->>FSM: hotkey release
    FSM->>BE: stop (via StopHandle)
    BE->>BE: poll stop ≤100 ms → quit + drain
    BE->>SRC: finish(None) — clean end, no Err (C8)
    SRC-->>FSM: stream ends
    INF-->>FSM: transcription.done
```

## 3. Sequence: graph-side format conversion (C2, FR-003)

The backend requests the **negotiated** format on the stream; when the device's
native rate/channels differ, the PipeWire graph converts — the crate never
resamples (adapters-never-resample invariant).

```mermaid
sequenceDiagram
    participant BE as PipeWireBackend
    participant G as PipeWire graph
    participant DEV as Device (e.g. 48 kHz stereo)

    BE->>G: connect Stream, SPA audio-raw = 16 kHz mono S16LE
    G->>DEV: open at device-native format
    DEV-->>G: 48 kHz stereo frames
    G->>G: resample + downmix to 16 kHz mono S16LE (§7)
    G-->>BE: process callback: frames EXACTLY negotiated format
    Note over BE: no rubato, no in-crate DSP (non-goal)<br/>width guard rejects non-S16 up front (C12)
```

## 4. Sequence: device selection by stable node.name (C3, C5) + limitations (C4, C10)

Selection uses `PW_KEY_TARGET_OBJECT` (stable `node.name`), so it is
renumber-invariant by construction. `DONT_RECONNECT` is set when a target is
given, so a *chosen* device that vanishes faults. An **absent-at-start** bogus
target is a documented platform limitation (WirePlumber falls back to default).

```mermaid
sequenceDiagram
    participant FSM as Consumer
    participant BE as PipeWireBackend
    participant WP as WirePlumber / graph
    participant N as Node "alsa_input…Mic2"

    FSM->>BE: CaptureSpec.target = "alsa_input…Mic2" (node.name)
    BE->>WP: connect Stream, PW_KEY_TARGET_OBJECT = node.name, DONT_RECONNECT
    WP->>N: link to that exact node (C3)
    Note over WP,N: graph renumbers volatile ids → same node.name still links (C5)

    alt chosen node vanishes mid-capture
        N--)WP: node removed
        WP-->>BE: stream error (no reconnect)
        BE->>FSM: finish(Some(DeviceUnavailable)) — one Err, then end (C10)
    else target absent at connect (bogus name)
        Note over WP: DEFAULT WirePlumber policy falls back to default source<br/>(pw-record does the same) → captures, no fault.<br/>Documented limitation, not a backend defect (spec §Clarifications, C4)
    end
```

## 5. State: `PipeWireBackend` lifecycle → exactly one terminal `finish` (C8–C11)

```mermaid
stateDiagram-v2
    [*] --> Configured: new()
    Configured --> Capturing: start() connects Stream (Ok)
    Configured --> Finished: open fails ⇒ Err(DeviceUnavailable|Backend) from start (FR-010)

    Capturing --> Stopped: graceful stop (drain first, FR-011)
    Capturing --> Aborted: CaptureStream dropped / ring closed
    Capturing --> Faulted: stream error / device lost (FR-010)

    Stopped --> Finished: finish(None)
    Aborted --> Finished: finish(None) + ring discarded
    Faulted --> Finished: finish(Some(CaptureError))
    Finished --> [*]

    note right of Capturing
        stop/abort observed ≤ 250 ms (FR-012, C11)
        via the ≤100 ms loop poll timer
    end note
```

## 6. Flow: per-`process`-callback data path (C6, C12, C13)

```mermaid
flowchart TD
    A["process callback (RT)<br/>interleaved S16 frames, negotiated rate"] --> B{"sample_width == 2 bytes?"}
    B -- no --> Z["Err(UnsupportedFormat) at start (C12)"]
    B -- yes --> C{"spec.channels = Some(idx…)?"}
    C -- no --> E["frames as-is (graph already mono/negotiated)"]
    C -- yes --> D["request max(idx)+1 graph channels;<br/>select requested indices, average → negotiated layout (C6, §9)"]
    D --> E
    E --> F["Producer::push(bytes) — non-blocking"]
    F --> G{"ring full?"}
    G -- no --> H["enqueue; stats tap updates<br/>captured += chunk (C13)"]
    G -- yes --> I["drop OLDEST; stats.dropped += chunk<br/>(healthy session ⇒ dropped == 0, SC-006)"]
    I --> H
    H --> J["consumer drains CaptureStream after 'ready'"]
```

## 7. Sequence: live input-device enumeration (E1–E4, FR-008/FR-008a)

`InputDevices` runs a registry listener on its own loop thread; `list()` is the
current snapshot, `watch()` publishes the latest full list as devices appear and
disappear — a chooser stays current without polling. Read-only, no audio (E6).

```mermaid
sequenceDiagram
    participant UI as Chooser (--list-devices)
    participant D as InputDevices (loop thread)
    participant R as PipeWire registry

    UI->>D: InputDevices::new()
    D->>R: subscribe to globals
    R-->>D: current globals (Audio/Source, has node.name, monitors excluded)
    UI->>D: list()
    D-->>UI: Vec<InputDevice>{node_name, label}  (empty ⇒ Vec, not Err — E2)

    UI->>D: watch()
    D-->>UI: watch::Receiver (latest full list)
    R--)D: global (AirPods appears)
    D-->>UI: watch update: list + new device (E3)
    R--)D: global_remove (device gone)
    D-->>UI: watch update: list without it (E4, by stable node_name)

    Note over UI,D: a node_name here feeds CaptureSpec.target directly (E7)
    UI->>D: drop handle ⇒ listener stops, loop thread released
```
