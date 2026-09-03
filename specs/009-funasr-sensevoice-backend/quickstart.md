# Quickstart: Validating the FunASR / SenseVoice Backend

**Feature**: `specs/009-funasr-sensevoice-backend`

Prerequisites: English corpus fetched (`dev/fetch_english_corpus.py`),
Chinese corpus fetched (`dev/fetch_chinese_corpus.py`), `client/` built
(`cargo build --release`), FunASR model cache populated.

## S1 — Adapter in myna-server (internal dialect)

Fetch the model (first-time, offline thereafter):

```sh
python dev/fetch_funasr_model.py
```

Start the server:

```sh
uv run myna-server --socket /tmp/myna.sock --adapter funasr
```

Expected lifecycle on server stdout: warm-up log line, then `ready` gate
open.

Dictate an English utterance from the real corpus, internal dialect:

```sh
./client/target/release/myna-dictate --socket /tmp/myna.sock \
    --dialect internal --clip corpus/real/audio/librispeech-2277-149896-0005.wav
```

Expected: `✓` with the transcript; no model control tags (`<|zh|>`, etc.)
in the output. The result is unpunctuated.

Repeat with the IE115 dialect:

```sh
./client/target/release/myna-dictate --socket /tmp/myna.sock \
    --dialect ie115 --clip corpus/real/audio/librispeech-2277-149896-0005.wav
```

Expected: same transcript; IE115 event flow (`completed`) with zero protocol
errors.

## S2 — Chinese dictation

```sh
./client/target/release/myna-dictate --socket /tmp/myna.sock \
    --dialect internal --clip corpus/chinese/audio/<pick_a_clip>.wav
```

Expected: Chinese transcript; auto-detected language (no `--language` flag
needed). Output is unpunctuated Chinese text.

Pin language:

```sh
myna-server --socket /tmp/myna.sock --adapter funasr --funasr-language zh
```

Re-run the same clip — same result (model was already auto-detecting zh).

## S3 — Capabilities discovery

```sh
uv run myna-server --socket /tmp/myna.sock --adapter funasr &
sleep 2
# capabilities.query is a raw control frame; use the test harness:
python -c "
import asyncio
from myna.core.transport_ws import send_and_receive
asyncio.run(send_and_receive('/tmp/myna.sock'))
"
```

Expected: `Capabilities(models=('sensevoice-small',), languages=('auto',
'zh', 'en', 'yue', 'ja', 'ko'), punctuation=False, translation=False)`.

## S4 — Accuracy evaluation

```sh
# English (vs whisper-tiny baseline — SC-002)
dev/bench.py --adapter funasr --corpus real

# Chinese (SC-001)
dev/bench.py --adapter funasr --corpus chinese
```

Expected: English WER within few pp of whisper-tiny; Chinese CER close to
published SenseVoice-Small figures. Output recorded in
`results/bench-*.json`.

## S5 — Tag stripping audit (SC-006)

```sh
python -c "
import json, re
with open('results/bench-funasr-chinese.json') as f:
    data = json.load(f)
for entry in data:
    for k in ('hypothesis', 'reference'):
        text = entry.get(k, '')
        if re.search(r'<\|.*?\|>', text):
            print(f'FAIL: {k} has residual tags: {text}')
            break
    else:
        continue
    break
else:
    print('PASS: zero residual control tags')
"
```

Expected: `PASS: zero residual control tags`.

## S6 — Confined snap (US2)

Build and install:

```sh
(cd funasr-snap && ./dev/prepare.sh && snapcraft --use-lxd)
sudo snap install --dangerous myna-funasr_*.snap myna-funasr+model-sensevoice.comp
```

End-to-end with confined client:

```sh
sudo snap connect myna:ubustt-socket myna-funasr:ubustt-socket
./client/target/release/myna-dictate \
    --socket /var/snap/myna-funasr/common/run/ubustt.sock \
    --clip corpus/real/audio/librispeech-2277-149896-0005.wav
```

Expected: transcript returned; network can be disabled (no `network` plug);
snap reports `ready` after warm-up.

## S7 — Idle-unload

```sh
# Let the server idle past the unload timeout
sleep 120  # or whatever modelctl idle-unload interval
# Next session should show `preparing` → `ready` again (re-load + warm-up)
./client/target/release/myna-dictate --socket /var/snap/myna-funasr/common/run/ubustt.sock \
    --clip corpus/real/audio/librispeech-2277-149896-0005.wav
```

Expected: session succeeds after re-load; warm-up log message visible in snap
journal.
