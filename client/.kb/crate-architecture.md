# Preface

Read this document when changing Rust workspace dependencies or deciding which crate should own new client behavior.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

Solid arrows are Cargo dependencies. Dashed arrows are runtime integration boundaries.

```mermaid
flowchart TB
    subgraph workspace["client/ Cargo workspace"]
        core["myna-core<br/>contract types, wire codecs, settings"]
        audio["myna-audio<br/>PipeWire capture and devices"]
        orchestrator["myna-orchestrator<br/>session and residency FSMs"]
        cli["myna-cli<br/>myna-dictate"]
        desktop["myna-desktop<br/>activation, controller, injection"]
        hud["myna-hud<br/>focus-safe GTK renderer"]

        audio -->|"depends on"| core
        orchestrator -->|"depends on"| core
        cli -->|"depends on"| core
        cli -->|"depends on"| audio
        cli -->|"depends on"| orchestrator
        desktop -->|"depends on"| core
        desktop -->|"depends on"| audio
        desktop -->|"depends on"| orchestrator
    end

    pipewire["PipeWire"] -.->|"capture and device discovery"| audio
    backend["Inference backend<br/>Unix socket"] <-.->|"session events and PCM"| orchestrator
    gsettings[("GSettings")] -.->|"preferences"| core
    portal["GlobalShortcuts portal<br/>or control socket"] -.->|"activation"| desktop
    ibus["IBus"] <-.->|"focus and committed text"| desktop
    desktop -.->|"D-Bus status"| hud
    shell["GNOME Shell extension"] -.->|"hosts and positions"| hud
```

`myna-core` is the lowest internal layer and must not depend on higher-level crates. `myna-audio` and `myna-orchestrator` are peers over the core contract. Product binaries compose those boundaries; `myna-hud` remains a separate process and consumes status over D-Bus rather than linking to `myna-desktop`.
