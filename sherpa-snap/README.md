# sherpa-snap — sherpa-onnx streaming inference snap

Small CPU-tier streaming snap: a NeMo-family FastConformer transducer (k2-fsa
int8 ONNX export, English, 480 ms latency) served by sherpa-onnx
`OnlineRecognizer`.

Streaming is enabled by default: the recognizer emits unstable partials plus
endpoint-driven committed segments. The model is English-only and decodes
lowercase, unpunctuated text - its vocabulary is 1025 tokens whose only
punctuation is an apostrophe.

## Punctuation

A second component (`model-punct-en`, 7.5 MB) restores punctuation and
capitalisation over committed text: k2-fsa's conversion of
[Edge-Punct-Casing](https://github.com/frankyoujian/Edge-Punct-Casing)
(Apache-2.0), a CNN-BiLSTM that takes text and returns text. It never sees
audio. Turn it off with `modelctl set punctuation=false` to get the raw
transducer output back.

Where it runs follows the emission mode, because punctuation wants a whole
sentence and the streaming contract never restates a committed segment:

| Mode | Pass | Result |
|---|---|---|
| streaming (default) | once per committed segment | punctuated as you dictate, but a segment is punctuated without the next one's context, so most sentence-final marks are lost |
| batch (`streaming=false`) | once over the whole transcript | full context, correct sentence breaks |

Measured on 20 s of dictated LibriSpeech, same audio through both:

```text
streaming: Then he rang the bell, no answer The long drizzle had begun,
           pedestrians had turned up collars and trousers at the bottom …
batch:     Then he rang the bell no answer the long drizzle had begun.
           Pedestrians had turned up collars and trousers at the bottom. …
```

Closing that gap in streaming mode means holding each segment until the next
one endpoints, punctuating with one segment of lookback, and committing a
segment late. That is a latency trade against the dictation cadence and has
not been made - it is the outstanding follow-up here.

Partials are never punctuated: they are display-only and re-emitted on every
decode, so the pass would run ~20x more often on text about to be replaced,
and the case of a word would flicker as the hypothesis extends.

## Build

```bash
./dev/prepare.sh
./dev/download-models.sh
snapcraft pack
```

## Install

```bash
sudo snap install --dangerous \
    ./myna-sherpa_*.snap \
    ./myna-sherpa+model-fastconformer-480ms.comp \
    ./myna-sherpa+model-punct-en.comp
```

The snap is CPU-only and does not require `hardware-observe`. Session socket:

```text
/var/snap/myna-sherpa/common/run/ubustt.sock
```
