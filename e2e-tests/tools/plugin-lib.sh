# shellcheck shell=bash
# VM-side helpers for the plugins suite, on top of dictation-lib.sh. Source
# inside the VM as the autologin user: `. myna-shot/plugin-lib.sh`.

# shellcheck source=e2e-tests/tools/dictation-lib.sh
. "$HOME/myna-shot/dictation-lib.sh"

BLOBS=$HOME/myna-shot/blobs

# What the daemon says it resolved: bound models, missing requests, steps.
daemon_status() { snap run myna --status 2>&1; }
status_has() { daemon_status | grep -q -- "$1"; }
daemon_pid() { systemctl --user show -P MainPID snap.myna.myna.service; }

# component_installed SNAP COMPONENT
component_installed() {
    snap components "$1" 2>/dev/null | awk -v c="$1+$2" '$1 == c && $2 == "installed" { found = 1 } END { exit !found }'
}

# What the CLI sideload does: one authorization, no store. The stand-in for
# Settings' Install while snapd refuses components of a sideloaded snap.
sideload() { sudo snap install --dangerous "$@"; }

# The first install of a text snap, as the set-up path leaves it: the snap
# present and its provider connected, no model component yet. A sideload gets
# none of the store's plug grants: hardware-observe (myna-text declares it for
# onnxruntime's PCI scan) is connected here, or its denial fails the run.
seed_text_snap() { # SNAP FILE
    sideload "$BLOBS/$2" && sudo snap connect myna:backend "$1:provider" || return 1
    if snap connections "$1" | grep -q "$1:hardware-observe"; then
        sudo snap connect "$1:hardware-observe"
    fi
}

# polkit authorizations the e2e rule (tools/shot.sh) granted since EPOCH.
polkit_grants() {
    sudo journalctl -t myna-e2e-polkit --no-pager --since "@$1" -o cat 2>/dev/null | grep -c '^io.snapcraft.snapd.'
}

# A text provider Myna does not curate, as a sideloaded snap.
rogue_install() {
    local dir=$HOME/myna-shot/rogue
    mkdir -p "$dir/meta" "$dir/bin"
    cat > "$dir/meta/snap.yaml" <<'YAML'
name: myna-rogue-text
version: "1"
summary: A text provider Myna does not curate (e2e fixture)
description: Serves myna-text-v1 with a cleanup model that shouts its output.
base: core24
confinement: strict
grade: devel
apps:
  server:
    command: bin/server.py
    daemon: simple
    plugs: [network-bind]
slots:
  provider:
    interface: content
    content: inference-provider
    source:
      write: [$SNAP_COMMON/share/provider]
YAML
    install -m 0755 "$HOME/myna-shot/rogue-text.py" "$dir/bin/server.py"
    snap pack "$dir" "$dir" >/dev/null && sideload "$dir"/myna-rogue-text_1_all.snap \
        && sudo snap connect myna:backend myna-rogue-text:provider
}

STORE_FILE=$HOME/snap/myna/common/.config/glib-2.0/settings/keyfile
HISTORY_FILE=$HOME/snap/myna/common/processing-history.jsonl

# chain_set STEPS: one active chain of STEPS, a GVariant a(sba{ss}) literal
# such as "[('tidy', true, {})]", fit to the field off and the record of past
# dictations on, written the way GSettings writes its keyfile (a rename),
# which the daemon reads live.
chain_set() {
    python3 - "$STORE_FILE" "$1" <<'PY'
import os, sys
path, steps = sys.argv[1:]
group = "[com/canonical/myna/dictation]"
want = {
    "chains": f"[('e2e', 'E2E', {steps})]",
    "active-chain": "'e2e'",
    "fit-to-field": "false",
    "processing-history": "true",
}
lines = open(path).read().splitlines() if os.path.exists(path) else []
if group not in lines:
    lines.append(group)
out, seen, ingroup = [], set(), False
for line in lines:
    if line.startswith("["):
        if ingroup:
            out += [f"{k}={v}" for k, v in want.items() if k not in seen]
        ingroup = line == group
    elif ingroup and "=" in line and line.split("=", 1)[0] in want:
        key = line.split("=", 1)[0]
        seen.add(key)
        line = f"{key}={want[key]}"
    out.append(line)
if ingroup:
    out += [f"{k}={v}" for k, v in want.items() if k not in seen]
os.makedirs(os.path.dirname(path), exist_ok=True)
open(path + ".tmp", "w").write("\n".join(out) + "\n")
os.rename(path + ".tmp", path)
PY
}

history_clear() { : > "$HISTORY_FILE"; }

# dictate_once: one whole dictation into the plain field, as a person's:
# toggle, a second of speech, toggle, wait for the daemon to be idle.
dictate_once() {
    field_clear && field_focus other && field_focus plain || return 1
    dictation_toggle && wait_until 10 dictation_state_is recording || return 1
    sleep 1
    dictation_toggle && wait_until 20 dictation_state_is idle
}

# dictate_many N WANT: N dictations; fails if the field ever lacks WANT.
dictate_many() {
    local i bad=0
    for i in $(seq "$1"); do
        if ! { dictate_once && wait_until 5 field_is plain "$2"; }; then
            bad=$((bad + 1)); echo "dictation $i did not type: $(field_get plain)" >&2
        fi
    done
    [ "$bad" = 0 ]
}

# field_matches REGEX: the plain field holds text matching it, case aside.
field_matches() { field_get plain | grep -qi -- "$1"; }

# dictate_many_matching N REGEX: as dictate_many, for text matching REGEX.
dictate_many_matching() {
    local i bad=0
    for i in $(seq "$1"); do
        if ! { dictate_once && wait_until 5 field_matches "$2"; }; then
            bad=$((bad + 1)); echo "dictation $i did not type: $(field_get plain)" >&2
        fi
    done
    [ "$bad" = 0 ]
}

FIRSTS_FILE=$HOME/myna-shot/out/firsts.jsonl

# firsts_collect N: the first dictation after each of N daemon starts, as
# soon as the daemon is up and the field is focused; their records are kept.
firsts_collect() {
    local i
    : > "$FIRSTS_FILE"
    for i in $(seq "$1"); do
        history_clear
        # A tight loop of restarts trips the unit's start limit (5 in 10 s).
        sleep 3
        systemctl --user reset-failed snap.myna.myna.service
        daemon_restart && field_focus other && field_focus plain || return 1
        dictation_toggle && wait_until 10 dictation_state_is recording || return 1
        sleep 1
        dictation_toggle && wait_until 20 dictation_state_is idle || return 1
        wait_until 5 test -s "$HISTORY_FILE"
        sleep 0.5
        cat "$HISTORY_FILE" >> "$FIRSTS_FILE"
    done
}

# dead_air_stats NAME P50MAX P95MAX: the wait that was not the speech model's
# (dead_air_ms - asr_tail_ms) of every piece in the record, with its three
# parts; fails past the p95 bound.
dead_air_stats() {
    python3 - "$1" "$2" "$3" "$HISTORY_FILE" <<'PY'
import json, sys
name, p50max, p95max, path = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), sys.argv[4]
rows = [json.loads(line) for line in open(path) if line.strip()]
timed = [r for r in rows if "dead_air_ms" in r and "asr_tail_ms" in r]
if not timed:
    sys.exit(f"{name}: no timed dictation in the record ({len(rows)} lines)")
rest = sorted(r["dead_air_ms"] - r["asr_tail_ms"] for r in timed)
inject = sorted(r.get("inject_ms", 0) for r in timed)
pct = lambda xs, p: xs[min(len(xs) - 1, int(p * len(xs) + 0.999999) - 1)]
print(f"STAT {name}: n={len(rest)} (dead_air - asr_tail) ms p50={pct(rest, .5)} p95={pct(rest, .95)} max={rest[-1]}; inject p95={pct(inject, .95)}")
sys.exit(0 if pct(rest, .95) < p95max and pct(rest, .5) < p50max else 1)
PY
}

# dead_air_firsts NAME MAX: every timed piece of the dictations that
# firsts_collect made; fails if any waited MAX ms or more past the speech
# model's tail.
dead_air_firsts() {
    python3 - "$1" "$2" "$FIRSTS_FILE" <<'PY'
import json, sys
name, bound, path = sys.argv[1], float(sys.argv[2]), sys.argv[3]
rows = [json.loads(line) for line in open(path) if line.strip()]
timed = [r for r in rows if "dead_air_ms" in r and "asr_tail_ms" in r]
if not timed:
    sys.exit(f"{name}: no timed dictation")
rest = [r["dead_air_ms"] - r["asr_tail_ms"] for r in timed]
print(f"STAT {name}: first dictation after each start, (dead_air - asr_tail) ms per piece: {rest}")
sys.exit(0 if max(rest) < bound else 1)
PY
}

# fault_install: a text provider that passes for myna-text (the curated name)
# and answers wrongly on command: the mode is the word in its common dir.
fault_install() {
    local dir=$HOME/myna-shot/fault
    mkdir -p "$dir/meta" "$dir/bin"
    cat > "$dir/meta/snap.yaml" <<'YAML'
name: myna-fault-text
version: "1"
summary: A faulty text provider (e2e fixture)
description: Serves myna-text-v1 as myna-text and fails on command.
base: core24
confinement: strict
grade: devel
apps:
  server:
    command: bin/server.py
    daemon: simple
    plugs: [network-bind]
slots:
  provider:
    interface: content
    content: inference-provider
    source:
      write: [$SNAP_COMMON/share/provider]
YAML
    install -m 0755 "$HOME/myna-shot/fault-text.py" "$dir/bin/server.py"
    snap pack "$dir" "$dir" >/dev/null && sideload "$dir"/myna-fault-text_1_all.snap \
        && sudo snap connect myna:backend myna-fault-text:provider
}
fault_mode() { echo "$1" | sudo tee /var/snap/myna-fault-text/common/mode >/dev/null; }
