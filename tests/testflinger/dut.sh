#!/bin/bash
# The device half of the Testflinger nightly, run as root on the lab machine:
# install what a user installs from latest/edge, then the adapter smoke.
set -eux
snap set system experimental.user-daemons=true
snap install --edge myna
snap install --edge myna-whisper+model-tiny
SNAP_UNDER_TEST=myna-whisper MODELCTL=myna-whisper.whisper MODEL=tiny ./smoke.sh
snap list
