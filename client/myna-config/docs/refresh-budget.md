# Refresh policy and idle resource budget

The configuration UI is **event-driven**. A refresh cycle may spawn at most:

| Reason                     | Process budget (upper bound)                                |
| -------------------------- | ----------------------------------------------------------- |
| Idle                       | `0`                                                         |
| Startup                    | `0`, plus three snapd socket reads: `/v2/snaps`, `/v2/connections` and `/v2/changes?select=in-progress` |
| Window regains focus       | as Startup, at most once per `FOCUS_REDISCOVERY_INTERVAL` (2 s) |
| Backend selected           | `BACKEND_REFRESH_PROCESS_BUDGET` (9 = `snap info` + at most 4 prioritized app probes + 4 modelctl reads) |
| Diagnostics requested (n backends) | n * `BACKEND_REFRESH_PROCESS_BUDGET`, plus the Startup reads |
| Report an Issue (n backends) | n * `BACKEND_REFRESH_PROCESS_BUDGET`: the report describes every backend, and the Diagnostics tab may never have read them |
| Apply running              | `0`, plus one snapd socket read per `APPLY_PROGRESS_INTERVAL` (1 s) |

Startup additionally pays one startup-sized assessment before any window
exists, to decide between the settings window and the onboarding wizard
(`docs/onboarding.md`). The result is handed to the wizard rather than
re-read there. The wizard re-assesses, at the same cost, each time it regains
focus on its component step, and every 2 s while that step shows and something
is still missing, because an install may also run outside it (a terminal, a
wizard since closed) while it keeps the focus. Setting up from that step reads snapd's changes
over its socket, spawning nothing, and again every 2 s, for at most 15 min, while snapd still has a change in
progress on Myna or a backend.

The settings window rediscovers each time it regains focus, because the user
may install or remove models in App Center or a terminal. An install from the
Install more models dialog reads its snapd change every second until it is
done, then starts a discovery of its own; every discovery and opening the
dialog read snapd's changes once, to follow an install already running, since
snapd lists a model before its install is done (#51). A backend switch reads
them once more before it starts, and refuses while snapd is changing a snap it
touches. It is refused while a switch or apply holds the
operation gate (that operation rediscovers when it completes), while any
discovery runs, and within `FOCUS_REDISCOVERY_INTERVAL` of any discovery
starting, so a window flapping in and out of focus reads snapd at most twice
per interval. Any other discovery supersedes it. It shows no loading state,
redraws only what changed (an open Install more models dialog included), and
skips the clock probe and the Diagnostics snapshots below; the controller
tests in `src/backend_controller.rs` cover the throttle.

Every other discovery also runs the CPU clock probe (`docs/performance-warnings.md`)
on the blocking pool. It is a thread, not a process, so it is outside the
budget above; it loads one core for 300 ms per frequency class and finishes
before the snapd reads it runs alongside.

While a Model-tab apply runs, the page polls snapd once per
`APPLY_PROGRESS_INTERVAL` (1 s) with `GET /v2/changes?select=in-progress` over
`/run/snapd.socket`, as the user, on the blocking pool: no process, no prompt.
It shows the running download's bytes ("Downloading model-small: 210.0 MB of
484.0 MB") or else the change's summary on the row the user changed, updating
it in place so the page keeps its scroll and focus. The listing is filtered here by the tasks' `affected-snaps`, because
snapd's own `for=<snap>` returns nothing for the `snapctl-install` change a
model download runs in. The poll stops when the apply does, and the privileged
plan has no deadline: a 484 MB download took about five minutes on the Noble
test box.

Apart from that step and a running apply there is **no background poll**. Refreshes are triggered
by (a) startup, (b) selecting the Backend or Diagnostics tab, (c) a user tap on the diagnostics *Refresh* button, (d) explicit
apply/switch operations, (e) the settings window regaining focus, and (f) a `PropertiesChanged` signal on the dictation
interface for a property Settings shows. Its `Get` is per property: `GetAll` would hand Settings the dictated text
(`DraftText`, `DraftTail`) published beside them, which it never reads. User-initiated diagnostics refreshes are debounced by
`diagnostics::REFRESH_DEBOUNCE` (250 ms).

The app-probe cap does not truncate the raw `snap info` command list. Discovery
first inspects all advertised names in-process, moves names matching supported
model-control conventions (the backend's own app name, `modelctl`, `control`,
`config`, or `settings`) ahead of unrelated commands, and only then probes at
most four candidates with `status --format=json`.

The backend budget is covered by
[`tests/backend_repository.rs`](../tests/backend_repository.rs), which counts
the subprocesses a snapshot read spawns.

## Reproducible idle CPU/RSS measurement

Because the UI never polls in the background, the following procedure gives a
deterministic idle footprint:

```sh
# 1. Build the release binary.
cargo build --release -p myna-config

# 2. Launch under an isolated Xvfb. The inner shell records the application
#    PID (rather than the xvfb-run wrapper), waits for startup, and samples
#    CPU %, RSS KiB, and direct child count every 5 seconds for one minute.
xvfb-run -a -s "-screen 0 1024x768x24" sh -c '
  ./target/release/myna-config &
  ui_pid=$!
  sleep 5
  for i in $(seq 1 12); do
    children=$(pgrep -P "$ui_pid" | wc -l)
    ps -o pid=,pcpu=,rss= -p "$ui_pid"
    printf "children=%s\n" "$children"
    sleep 5
  done
  kill "$ui_pid"
  wait "$ui_pid" 2>/dev/null || :
'
```

Measured on the Ubuntu Xvfb development host on 2026-09-05 (12 samples after
the five-second settling delay):

* **CPU**: `4.8 %` first sample, decaying to `0.4 %` by the final sample
  (`1.36 %` average of cumulative `ps` CPU readings).
* **RSS**: `117,380–117,492 KiB` (`~114.7 MiB`), stable after startup.
* **Child process count**: `0` after the initial discovery has completed.

The current regression watermarks on this environment are final-sample CPU
`<= 0.5 %`, RSS `<= 125 MiB`, and zero idle children. Re-measure and justify
changes rather than copying these host-specific numbers to a different SDK or
display server.

If the child count is ever non-zero without a user action, something has
started polling.
