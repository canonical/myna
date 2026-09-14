# Refresh policy and idle resource budget

The configuration UI is **event-driven**. `myna_config::diagnostics::RefreshPolicy`
is the single source of truth for when and how many subprocesses may be spawned
during a refresh cycle:

| Reason                     | Process budget (upper bound)                                |
| -------------------------- | ----------------------------------------------------------- |
| `Idle`                     | `0`                                                         |
| `Startup`                  | `APP_REFRESH_PROCESS_BUDGET` (2 = `snap list` + `snap connections`) |
| `BackendSelected`          | `BACKEND_REFRESH_PROCESS_BUDGET` (9 = `snap get` + `snap info` + at most 4 prioritized app probes + 3 modelctl reads) |
| `DiagnosticsRequested(n)`  | `2 + n * BACKEND_REFRESH_PROCESS_BUDGET`                    |

Startup additionally pays one `Startup`-sized assessment before any window
exists, to decide between the settings window and the onboarding wizard
(`docs/onboarding.md`). The result is handed to the wizard rather than
re-read there.

Every discovery also runs the CPU clock probe (`docs/performance-warnings.md`)
on the blocking pool. It is a thread, not a process, so it is outside the
budget above; it loads one core for 300 ms per frequency class and finishes
before the snapd reads it runs alongside.

`RefreshPolicy::periodic_interval()` is **always `None`**: there is no
background poll. Refreshes are triggered by (a) startup, (b) sidebar selection,
(c) a user tap on the diagnostics *Refresh* button, and (d) explicit
apply/switch operations. All user-initiated refreshes are debounced by
`RefreshPolicy::debounce()` (250 ms).

The app-probe cap does not truncate the raw `snap info` command list. Discovery
first inspects all advertised names in-process, moves names matching supported
model-control conventions (the backend's own app name, `modelctl`, `control`,
`config`, or `settings`) ahead of unrelated commands, and only then probes at
most four candidates with `status --format=json`.

The invariants above are covered by
[`tests/diagnostics_contract.rs`](../tests/diagnostics_contract.rs); the
`refresh_policy_has_no_idle_poll_and_enforces_process_budgets` test fails
loudly if a future change reintroduces perpetual polling.

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

If the child count is ever non-zero without a user action, the refresh policy
has regressed and the failing test cited above will pinpoint the culprit.
