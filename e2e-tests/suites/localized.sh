#!/usr/bin/env bash
# snapshot: ready
# Myna Settings reads a set-up machine as set up whatever the desktop
# language, though snap translates its own output there, and recommends the
# model for the desktop's language: FunASR, not yet installed, for Chinese.
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"

L=sv_SE.UTF-8
P=localized-$REL

# Positive control: without translated snap output this suite proves nothing.
assert_on "snap prints translated headers" \
    "LANG=$L snap list | head -1 | grep -qv '^Name'"
if shot --lang "$L" waitfor:Diagnostics shot:"$P"-settings.png; then
    echo "PASS: the settings window opened"
else
    echo "FAIL: the settings window did not open"; FAILED=1
fi
assert_on "onboarding found Myna and the model" \
    "grep 'onboarding assessment:' myna-shot/out/app.log | tail -1 | grep -q 'Myna found, Model found'"

# FunASR's pill shows only in the dialog; the installed list's goes to Whisper.
if tree=$(shot --lang zh_CN.UTF-8 \
    "waitfor:A better model for 中文 is available" \
    "click:Install more models" waitfor:FunASR shot:"$P"-zh-dialog.png dump); then
    echo "PASS: Install more models hints a better model for 中文"
else
    echo "FAIL: no hint of a better model for 中文"; FAILED=1
fi
if grep -q "\[list item\] 'FunASR' (.*Best for 中文" <<<"$tree"; then
    echo "PASS: FunASR is best for 中文"
else
    echo "FAIL: FunASR is not marked best for 中文"; FAILED=1
fi
suite_status
