# shellcheck shell=bash
# shellcheck disable=SC2034  # the suites that source this file are the users
# What the Processing tab's suites share: the rig (fake backend, virtual
# microphone, the field app), the keyfile they assert, and the dictation and
# store helpers they drive. Source it after lib.sh, do not execute it.
#
# A suite is one scenario on a provisioned base: `processing-page`,
# `processing-editor`, `processing-catalog`, `processing-language`,
# `chains-dictation` and `chains-locales`. Each copies the base itself, so they
# are independent and any one of them can be run alone.

REPO=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
FULL="The quick brown fox jumps over the lazy dog."
# What the English to French model says of FULL, in the words that matter.
FRENCH_WORDS='renard.*chien'
LIB='. myna-shot/plugin-lib.sh'
# shellcheck disable=SC2016 # expands in the VM
STORE='$HOME/snap/myna/common/.config/glib-2.0/settings/keyfile'
CHAIN="New chain"
# The daemon offers a model once it has bound its share (InstalledSteps).
OFFERS='gdbus call --session --dest com.canonical.Myna.Dictation --object-path /com/canonical/Myna/Dictation --method org.freedesktop.DBus.Properties.Get com.canonical.Myna.Dictation InstalledSteps'

step() { # NN name: a step header
    echo "-- $1. $2"
}

dict() { on "$LIB; $1"; }
assert_dict() { # description command
    if dict "$2" >/dev/null 2>&1; then echo "PASS: $1"; else echo "FAIL: $1"; FAILED=1; fi
}

evidence() { # NAME: what the run did, kept with the artifacts
    dict "cd myna-shot/out
        cp field.json \"field-$1.json\" 2>/dev/null
        daemon_status > \"status-$1.txt\" 2>&1
        grep -E '^(chains|active-chain)=' $STORE > \"keyfile-$1.txt\" 2>&1
        journalctl --user -u snap.myna.myna.service --no-pager -o short-precise > daemon.log 2>&1
        true" >/dev/null 2>&1
    return 0
}

dictate() {
    assert_dict "the field is cleared" 'field_clear'
    assert_dict "the plain field is focused afresh" 'field_focus other && field_focus plain'
    dict dictation_toggle
    assert_dict "recording" 'wait_until 10 dictation_state_is recording'
    dict dictation_toggle
    assert_dict "idle after the stop" 'wait_until 20 dictation_state_is idle'
}

now() { on 'date +%s'; }

# offered WHAT: wait for the daemon to publish a model it bound at startup in
# `InstalledSteps`. Until it does, Settings lists a model it has installed as
# one to install, which is the difference between Add and Install on a row.
offered() { # WHAT, in the daemon's own words
    local i
    for i in $(seq 60); do
        dict "$OFFERS | grep -q $1" && return 0
        sleep 1
    done
    return 1
}

# unoffered WHAT: the other way round - wait for the daemon to stop listing a
# model it bound, after one was removed. Until it does, the step that needed
# it still reads as installed and offers no Install.
unoffered() { # WHAT, in the daemon's own words
    local i
    for i in $(seq 120); do
        dict "$OFFERS | grep -q $1" || return 0
        sleep 1
    done
    return 1
}

# slowpoll: poll for up to 5 minutes, for a store download and a restart.
# shellcheck disable=SC2016 # expands in the VM
slowpoll() { printf 'sh:for _ in $(seq 1500); do { %s; } >/dev/null 2>&1 && exit 0; sleep 0.2; done; exit 1' "$1"; }

# rig: what every Processing scenario needs before it drives the page. `seeded`
# takes the base with the text components, `ready` the one without.
rig() { # seeded|ready
    step 0 "the rig: the fake speech backend, a virtual microphone, the field app"
    on 'mkdir -p myna-shot/out'
    lxc file push --uid 1000 --gid 1000 "$REPO/tests/spread/adapter-smoke/fixture.wav" \
        "$VM/home/ubuntu/myna-shot/speech.wav"
    assert_on "precondition: the speech backend is the fake" \
        'snap connections myna | grep -q "myna:backend.*myna-fake-backend:provider"'
    assert_dict "the virtual microphone is made the default source" 'mic_make_default'
    dict 'mic_start myna-shot/speech.wav'
    assert_dict "the virtual keyboard is up" 'keys_start'
    dict 'key escape'
    assert_dict "the daemon logs at the debug tier" 'daemon_debug on'
    assert_dict "the field app is up" 'field_start'
    assert_dict "the field is focused" 'field_focus plain'
    assert_dict "the dictation language is English" 'snap run myna.config set language en'
    case $1 in
        seeded)
            assert_dict "the text models are installed" \
                'component_installed myna-translation model-translate-en-fr && component_installed myna-text model-disfluency-en'
            # A fresh copy's daemon binds the installed models at startup.
            if offered disfluency; then
                echo "PASS: the daemon offers the seeded models"
            else
                echo "FAIL: the daemon does not offer the seeded models"; FAILED=1
            fi
            ;;
        ready)  assert_dict "no text model is installed" \
                    '! component_installed myna-translation model-translate-en-fr && ! component_installed myna-text model-disfluency-en' ;;
    esac
    assert_on "the chains are emptied" \
        "snap run myna.config set chains '[]' && snap run myna.config set active-chain ''"
}

# fresh_chain LANGUAGE TRANSCRIPT: no chains, the dictation language and what
# the fake backend hears.
fresh_chain() {
    assert_on "the chains are emptied" \
        "snap run myna.config set chains '[]' && snap run myna.config set active-chain ''"
    assert_on "the dictation language is $1" "snap run myna.config set language $1"
    assert_on "the fake backend hears the $1 sentence" \
        "sudo snap set myna-fake-backend transcript='$2' && for _ in \$(seq 60); do test -S /var/snap/myna-fake-backend/common/share/provider/myna.sock && exit 0; sleep 0.5; done; exit 1"
}

switch_on() {
    shot waitfor:General tab:Processing "click:Use $CHAIN" \
        "$(poll "! grep -q \"active-chain=''\" $STORE")" \
        || { echo "switch on failed" >&2; FAILED=1; }
}

# READY: the daemon says the active chain is ready to dictate.
# shellcheck disable=SC2016 # expands in the VM
READY='for _ in $(seq 120); do [ "$(dictation_prop ChainStatus)" = ready ] && exit 0; sleep 1; done; exit 1'
