# Audio Adapter Library Architecture

## Component Block Diagram

```mermaid
graph LR
    SC[Speech Controller / Consumer]
    API[Public API: enumerate_nodes, open_stream]
    REG[(Open Stream Registry)]
    BACKEND[AudioBackend trait]
    PW[PipeWire Backend]
    PA[PulseAudio Backend]
    MOCK[MockBackend]
    QUEUE[AudioQueue bounded buffer]
    CONVERT[ConversionPipeline]
    PRE[PreprocessPipeline]
    STREAM[AudioStream]

    SC --> API
    API --> REG
    API --> BACKEND
    BACKEND --> PW
    BACKEND --> PA
    BACKEND --> MOCK
    BACKEND --> QUEUE
    QUEUE --> STREAM
    STREAM --> SC
    BACKEND -.->|negotiate format| CONVERT
    CONVERT -.->|if needed| PRE
```

## Stream Open and First-Frame Delivery

```mermaid
sequenceDiagram
    participant C as Consumer
    participant API as open_stream
    participant B as AudioBackend
    participant Q as AudioQueue
    participant S as AudioStream

    C->>API: open_stream(config)
    API->>B: enumerate()
    API->>B: open(node, producer)
    B->>Q: start capture thread
    API->>S: create AudioStream(consumer)
    S->>C: return stream
    Note over B,Q: capture callback pushes frames
    C->>S: read_timeout()
    S->>Q: pop frames/events
    Q->>S: AudioFrame(s)
    S->>C: Vec<StreamItem>
```

## Consumer Read Loop with Overrun

```mermaid
sequenceDiagram
    participant C as Consumer
    participant S as AudioStream
    participant Q as AudioQueue

    loop read_timeout
        C->>S: read_timeout(timeout)
        alt buffer full, producer drops oldest
            Q->>Q: drop oldest frame(s)
            Q->>Q: record dropped_bytes
        end
        S->>Q: pop()
        alt dropped_bytes > 0
            Q->>S: Overrun event
        else frames available
            Q->>S: AudioFrame(s)
        end
        S->>C: Vec<StreamItem>
    end
```

## Underrun Silence Fill

```mermaid
sequenceDiagram
    participant B as AudioBackend
    participant Q as AudioQueue
    participant S as AudioStream
    participant C as Consumer

    Note over B: server delivers gap
    B->>Q: push_silence(gap_duration)
    Q->>Q: zero-fill + fade boundary
    S->>Q: pop()
    Q->>S: AudioFrame(silent) + Underrun event
    S->>C: Vec<StreamItem>
```

## Device Loss

```mermaid
sequenceDiagram
    participant B as AudioBackend
    participant Q as AudioQueue
    participant S as AudioStream
    participant C as Consumer

    Note over B: node disappears
    B->>Q: push_event(DeviceLost)
    B->>B: stop capture thread
    S->>Q: pop()
    Q->>S: DeviceLost event
    S->>C: Vec<StreamItem>
    Note over C: caller decides re-open
```

## Mid-Stream Format Renegotiation

```mermaid
sequenceDiagram
    participant B as AudioBackend
    participant C as ConversionPipeline
    participant Q as AudioQueue

    Note over B: source format changes
    alt server can provide target format
        B->>B: renegotiate server-side
    else fallback to in-process conversion
        B->>C: renegotiate_source(new_source)
        C->>C: rebuild resampler if rate changed
    end
    B->>Q: continue pushing target-format frames
```
