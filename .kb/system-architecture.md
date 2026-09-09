# Preface

Read this document for a visual overview of Myna's runtime components and trust boundaries. Use the subsystem diagrams for implementation-level structure.

Read the top-level `.kb/agents.md` file before continuing below.

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
    end

    subgraph inference["Inference backend snap"]
        socket["ubustt-socket<br/>content interface"]
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
    shell -->|"hosts"| hud

    snapd -->|"installs and connects"| client
    snapd -->|"installs components"| artifacts
    modelctl -->|"selects model and engine"| engine
```

The client is the only component with microphone and desktop access. Inference backends receive PCM through a connected local Unix socket and do not access the network or microphone. Unstable text remains presentation state; only committed text reaches IBus.
