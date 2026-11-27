# Preface

Read this document for a visual overview of Myna's runtime components and trust boundaries. Use the subsystem diagrams for implementation-level structure.

Read the top-level `.kb/agents.md` file before continuing below.

# Important

- Myna is offline: no component needs the network for its core function, and nothing falls back to a remote service or downloads at session time. `client/deny.toml` bans network client crates from the client.
- Captured audio lives only in bounded in-memory buffers and is never persisted by default. Transcript text never leaves the injection path: the D-Bus status, the HUD and logs carry state and energy only. The one exception is a `processing-chain` other than `exact`, the default: while one is chosen, the dictation in progress is published on the session bus as `DraftText` and `DraftTail` for the Shell extension to draw, and sent to the text model socket. It is still never logged, and persisted only while `processing-history` is on.

# Architecture

```mermaid
flowchart LR
    user([User])
    app["Focused application"]
    snapd["snapd"]

    subgraph desktop["Ubuntu desktop session"]
        pipewire["PipeWire"]
        client["Myna client snap<br/>myna-desktop"]
        settings[("GSettings<br/>keyfile store")]
        ibus["IBus"]
        shell["GNOME Shell extension"]
        hud["myna-hud"]
        hudhost["myna-hud-host<br/>Xfce/Plasma supervisor (myna-config deb)"]
    end

    subgraph inference["Inference backend snap"]
        socket["inference-provider<br/>content interface"]
        server["myna-server"]
        engine["Selected inference engine"]
        artifacts[("Model and runtime<br/>components")]
        modelctl["modelctl"]
    end

    user -->|"speaks"| pipewire
    user -->|"activates dictation"| client
    pipewire -->|"PCM audio"| client
    settings -->|"preferences"| client

    client -->|"WebSocket over Unix socket<br/>PCM frames"| socket
    socket --> server
    server --> engine
    artifacts --> engine
    server -->|"transcript events"| socket
    socket --> client

    client -->|"committed text"| ibus
    ibus --> app
    client -.->|"D-Bus state"| shell
    shell -->|"hosts (GNOME)"| hud
    hudhost -->|"supervises --host x11 / layer-shell"| hud
    hudhost -.->|"watches the daemon's bus name"| client

    snapd -->|"installs and connects"| client
    snapd -->|"installs components"| artifacts
    modelctl -->|"selects model and engine"| engine
```

The HUD has two hosts: on GNOME the Shell extension starts and positions it, and on Xfce and Plasma `myna-hud-host` (shipped in the myna-config deb, started by an XDG autostart entry) runs `myna-hud --host x11`, or `--host layer-shell` on Wayland, only while the daemon owns its D-Bus name, and the HUD places its own window.

The client is the only component with microphone and desktop access. Inference backends receive PCM through a connected local Unix socket and do not access the network or microphone. A backend shares its socket through an `inference-provider` content slot whose directory holds a `provider.env` naming the snap, the socket, and `MYNA_ROLE=speech` (`MYNA_ROLE=text` for Myna's text snap); the client partitions connected shares by role so text and LLM providers coexist alongside the one active speech provider, refusing to choose only when more than one speech provider serves. Unstable text remains presentation state; only committed text reaches IBus.
