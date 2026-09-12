# Preface

Read this document when changing the common structure or runtime flow of an inference snap. Individual snap manifests remain authoritative for model-specific differences.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

```mermaid
flowchart LR
    client["Myna client snap<br/>backend plug"]
    operator([Operator])
    snapd["snapd"]
    hardware["Host hardware metadata"]

    subgraph backend["One strictly confined inference snap"]
        slot["ubustt-socket<br/>writable content slot"]
        uds[("$SNAP_COMMON/run<br/>Unix socket")]

        subgraph control["Control plane"]
            cli["modelctl CLI"]
            hooks["install and refresh hooks"]
            config[("Snap configuration")]
            manifests["Engine, runtime, and<br/>model manifests"]
        end

        subgraph serving["Serving path"]
            service["server daemon"]
            launcher["Selected engine launcher"]
            myna_server["myna-server"]
            adapter["Model-family adapter"]
        end

        models[("Model components")]
        runtimes[("Optional runtime components")]
    end

    client -->|"WebSocket and PCM"| slot
    slot --> uds
    uds --> service
    service --> launcher
    launcher --> myna_server
    myna_server --> adapter
    adapter --> models
    runtimes --> launcher
    myna_server -->|"transcript events"| uds
    uds --> slot
    slot --> client

    operator -->|"list, select, configure"| cli
    cli --> config
    cli --> manifests
    hooks -->|"initial selection"| config
    config --> launcher
    hardware -.->|"hardware-observe"| cli
    snapd -->|"installs and refreshes"| backend
    snapd -->|"mounts components"| models
    snapd -->|"mounts components"| runtimes
```

The base snap carries the control CLI, manifests, launch scripts, and Python server. Model weights and large optional runtimes are components so they can be installed independently. `modelctl` selects an engine and model through persistent snap configuration; the daemon serves that selection through the shared Unix-socket contract.

# Important

- This is the common shape, not a requirement that every family have multiple engines or runtime components.
- The content interface is the only client/backend data path.
- `network-bind` permits listening on the Unix socket; it does not imply a TCP service.
- Inference snaps do not receive microphone or general network access.
