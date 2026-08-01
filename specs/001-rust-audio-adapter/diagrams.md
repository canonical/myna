# Diagrams: Audio Adapter Library

**Feature**: `001-rust-audio-adapter` | Satisfies FR-019; diagram 2 documents the known-consumer API surface (FR-020). Cross-references: [data-model.md](data-model.md), [contracts/audio-adapter-api.md](contracts/audio-adapter-api.md) (guarantees G1–G15).

## 1. Architectural block diagram

Components of `myna-audio-adapter` and their relationships. The real-time (RT) side runs on the backend's event-loop thread; the consumer side is whatever thread calls `read()`.

```mermaid
flowchart LR
    subgraph consumer["Consumer process (e.g. Speech Controller)"]
        SC["Consumer"]
    end

    subgraph crate["myna-audio-adapter"]
        FACADE["Facade<br/>enumerate_nodes()<br/>open_stream()"]
        STREAM["AudioStream<br/>read() / read_timeout() / close()"]

        subgraph rt["RT event-loop thread"]
            BE["AudioBackend trait"]
            PW["PipeWire backend<br/>(primary)"]
            PA["PulseAudio backend<br/>(fallback)"]
            CONV["Convert pipeline<br/>bypass | rubato resample<br/>+ mixdown/format"]
            PRE["Preprocess chain (optional)<br/>denoise → VAD → (deverb: deferred)"]
        end

        RING["Bounded SPSC ring buffer<br/>(≤ max_buffer_duration, default 10 s)<br/>drop-oldest + splice smoothing"]
    end

    subgraph server["Audio server"]
        SRV["PipeWire / PulseAudio"]
        NODES["Input nodes<br/>(mics, monitors, virtual)"]
    end

    SC -->|"open_stream(StreamConfig)"| FACADE
    FACADE --> BE
    BE --> PW
    BE --> PA
    PW <--> SRV
    PA <--> SRV
    SRV --- NODES
    PW --> CONV
    PA --> CONV
    CONV --> PRE
    PRE -->|"producer"| RING
    RING -->|"consumer"| STREAM
    STREAM -->|"Vec&lt;StreamItem&gt;: frames + events"| SC
```

## 2. Sequence: Speech Controller push-to-talk session over the consumer API surface (G15, FR-020)

How the known primary consumer (Speech Controller, `docs/architecture/UD129`) drives the adapter through one dictation session. Only the API surface shown here is consumer-facing; UD129 session states are on the left.

```mermaid
sequenceDiagram
    participant U as User
    participant SC as Speech Controller
    participant AA as Audio Adapter (public API)
    participant INF as Inference Snap

    Note over SC: settings time
    SC->>AA: enumerate_nodes()
    AA-->>SC: Vec<InputNode> (id, name, description, formats)

    U->>SC: hotkey press
    Note over SC: Starting
    SC->>AA: open_stream(StreamConfig)
    AA-->>SC: AudioStream (first frame ≤ 100 ms)
    Note over SC: Recording / Transcribing
    loop while hotkey held
        SC->>AA: read_timeout(d)
        AA-->>SC: [Frame | Event]
        SC->>INF: frame payloads (streaming)
        alt Event: VoiceActivity{speaking:false}
            SC->>INF: finalize / chunk utterance
        else Event: DeviceLost
            SC->>SC: end session, notify user
        else Event: Overrun / Underrun
            SC->>SC: record diagnostics (no raw audio)
        end
    end
    U->>SC: hotkey release
    Note over SC: Finalizing → Idle
    SC->>AA: close()
    AA-->>SC: source released, buffers cleared ≤ 200 ms
```

## 3. Sequence: stream open and first-frame delivery (G7, G9)

```mermaid
sequenceDiagram
    participant C as Consumer
    participant F as Facade
    participant B as Backend (PipeWire|Pulse)
    participant S as Audio server
    participant R as Ring buffer

    C->>F: open_stream(config)
    F->>F: node already open?
    alt already open (FR-003)
        F-->>C: existing AudioStream (no-op)
    else new stream
        F->>B: connect + create capture stream
        B->>S: negotiate target format (server-side convert preferred, FR-009)
        alt server honors target format
            S-->>B: stream in target format (convert = bypass)
        else server cannot honor
            S-->>B: stream in source format
            B->>B: enable in-process convert (rubato + mixdown)
        end
        S-->>B: audio quantum (RT callback)
        B->>R: converted target-format frames
        F-->>C: AudioStream (first frame readable ≤ 100 ms, SC-001)
    end
    C->>F: read()
    F-->>C: [Frame, Frame, ...]
```

## 4. Sequence: read loop with slow consumer — overrun (G3)

```mermaid
sequenceDiagram
    participant S as Audio server (RT)
    participant R as Ring buffer
    participant C as Consumer

    loop every audio quantum
        S->>R: push converted frames
    end
    Note over C: consumer stalls...
    S->>R: push (buffer at capacity)
    R->>R: drop OLDEST frames (FR-014)<br/>record dropped span<br/>smooth splice boundary (~5 ms fade, FR-015)
    C->>R: read()
    R-->>C: [Event: Overrun{dropped}, Frame, Frame, ...]
    Note over C: timeline stays ordered;<br/>loss is explicit, audio artifact-free
```

## 5. Sequence: audio-server underrun — silence fill (G4)

```mermaid
sequenceDiagram
    participant S as Audio server (RT)
    participant B as Backend
    participant R as Ring buffer
    participant C as Consumer

    S->>B: quantum n
    B->>R: frames (real audio)
    Note over S: server delivers nothing<br/>for a short span (underrun)
    B->>B: detect gap in capture clock
    B->>R: synthetic silence for missing span (FR-018)<br/>fade real→silence and silence→real (FR-015)
    S->>B: quantum n+k (audio resumes)
    B->>R: frames (real audio)
    C->>R: read()
    R-->>C: [Frame, Event: Underrun{filled}, Frame(silence), Frame, ...]
    Note over C: timeline continuous and wall-clock aligned;<br/>silent span flagged as synthetic
```

## 6. Sequence: input node lost mid-stream (G5)

```mermaid
sequenceDiagram
    participant S as Audio server
    participant B as Backend
    participant R as Ring buffer
    participant C as Consumer

    S--)B: node removed (registry event)
    B->>R: enqueue Event: DeviceLost{node}
    B->>B: close server stream,<br/>release source, stop RT processing (FR-016)
    C->>R: read()
    R-->>C: [...remaining frames, Event: DeviceLost]
    Note over C: stream is closed/terminal;<br/>consumer decides whether to re-open<br/>on another node — library never retargets
```

## 7. Sequence: mid-stream source format change — transparent renegotiation (G6)

```mermaid
sequenceDiagram
    participant S as Audio server
    participant B as Backend
    participant CV as Convert pipeline
    participant C as Consumer

    S--)B: param-changed: source now 44.1 kHz stereo
    alt convertible to target
        B->>CV: reconfigure (new src → target)
        CV-->>B: ready (no gap in output)
        Note over C: consumer sees uninterrupted<br/>target-format frames (FR-017) —<br/>no event, no state change
    else not convertible
        B->>C: Error: UnsupportedFormat (via read path)
        B->>B: close stream, release resources
    end
```

## 8. Flow: per-quantum data path (G1, G2, G12)

```mermaid
flowchart TD
    A["RT capture callback<br/>(source-format samples)"] --> B{"server delivered<br/>target format?"}
    B -- yes --> D{"preprocessing<br/>enabled?"}
    B -- no --> C["convert: resample (rubato)<br/>+ channel mixdown + sample format"]
    C --> D
    D -- no --> F["frame assembly<br/>(contiguous, seq++, timestamps)"]
    D -- yes --> E["stage chain:<br/>denoise → VAD(events) → (deverb)"]
    E --> F
    F --> G{"ring buffer<br/>full?"}
    G -- no --> H["push frames"]
    G -- yes --> I["drop oldest + smooth splice<br/>+ queue Overrun event"]
    I --> H
    H --> J["consumer read():<br/>Vec&lt;StreamItem&gt;"]
```
