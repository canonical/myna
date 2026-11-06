# Preface

Read this document before evaluating a model or a change to one (new
backend, quantisation, export, runtime or execution provider), treating
Parakeet's window-collapse rate as a port bug, tuning ONNX Runtime CUDA
execution, re-attempting an fp16 Parakeet export, changing Parakeet's keyword
biasing, giving Whisper or SenseVoice keywords, scoring keywords against the
audio, or changing the Nemotron streaming adapter
(export, quantisation, mel, the 3.5 multilingual model).

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

Findings from the 2026-09-17 Parakeet GPU workstream (fp32 CUDA export, NeMo
parity, fp16 attempt), the 2026-09-19 keyword-biasing tuning (Parakeet decode
bias) and its 2026-10-13 follow-up (post-correction, the Spanish tier), the
rejected Whisper keyword prompt, the September Nemotron 3.5 ASR evaluation,
and the 2026-11-02 Nemotron streaming ONNX adapter.
Numbers are
from zephyrus (RTX 4080 Laptop 12 GB, driver 595) on the 82-clip
`corpus/english/manifest-balanced.json` (+ 5-minute long-form clip) or FLEURS
(40 clips/language), unless noted.

# Important

## Evaluating a change

Decide first which question is being asked; they need different evidence.
Every number comes from `myna.benchmarker` runs that pass `make bench-check`
(`server/.kb/benchmarking.md`).

### Same model, different numerics

Quantisation, export, kernel, ONNX Runtime version or execution provider:
"did it break?". Against the shipped artifact:

1. Controls first: an identical rerun, and if possible a trivially
   perturbed one, give the churn floor everything else is read against.
2. Accuracy: `myna-bench compare` on the balanced corpus; the WER delta
   must not be worse beyond its interval. Read the per-clip diffs: noise is
   symmetric and sits on short or marginal clips, damage is directional
   (clean clips regress with nothing offsetting).
3. Failure modes: each known pathology's rate is not worse, and its churn
   is symmetric. A skew is a signal even in the good direction: the
   rejected 2026-08 T07 encoder collapsed less (8 windows regressed, 29
   fixed) while its WER got systematically worse.
4. Speed and resources: distributions over repeats, never one number.
5. Runtime nets such as `_transcribe_guarded` stay regardless.

Never gate a knife-edge metric on zero regressions. Collapse flips on
ulp-scale differences (a metadata-only kernel swap moved ~10 of 557
windows both ways; a rerun moved none), so a zero-regression bar admits only
bit-identical changes. A probe that only counts failures cannot localise
them; per-clip WER on sentinel clips with bisection over the change's parts
can (it found the one damaging FFN node of eleven).

### New model or backend

"Is it better for users?":

1. Validate the pipeline: a full publication preset must land within
   interval of the model's published numbers; if not, suspect our
   adapter, frontend or scoring before the model.
2. Product numbers: WER/CER per language and `--by-category`, finalize
   latency at `pace: realtime`, cold load, RAM/VRAM and J per audio-second,
   on each hardware tier the engine targets. `USABILITY_FAIL` is a result.
3. Find the model's own pathology before trusting its averages, then probe
   it: Parakeet goes blank over windows (below), Fun-ASR-Nano's int8
   export truncates past ~24 s and hallucinates on noise. Long-form and noise categories catch some
   of this; the balanced corpus has no speech-free clip, so hallucination on
   silence needs its own clips.

Collapse is probed by `dev/parakeet/collapse_probe.py`, outside the
benchmarker (no guard, provenance or `compare`); fold failure-mode probes
into the benchmarker when the next evaluation needs one.

## Parakeet v3 window collapse is the model's

A window collapses when it yields under 0.5 tokens per second of audio
(`_COLLAPSE_WORDS_PER_SECOND`, shared by the probe and the runtime retry).
It is a window-boundary effect, not a per-clip one.

557 sliding windows (3-16 s) over concatenated real speech: NeMo PyTorch
collapses 58/557 (10.4%), our ONNX fp32 60/557 (10.8%), 51 windows collapse
in both. The port is not the cause; keep the adapter's retry
(`_transcribe_guarded` in `myna/testbed/parakeet.py`) rather than chasing
this as an export or runtime bug. Reproduce with `dev/parakeet/collapse_probe.py`
run once per backend and diff the collapsed-window sets.

## fp32 ONNX parity with NeMo PyTorch

- fp32 ONNX transcripts equal NeMo PyTorch: 0.00% WER between them, 82 clips.
- Our `encoder-model.onnx.data` is byte-identical to istupakov's HF upload
  (also a NeMo export).
- onnx-asr's `nemo128.onnx` mel frontend matches NeMo's own frontend except
  at the edge frames.

## CUDA execution

- Each new input shape pays ONNX Runtime kernel-setup cost: streaming
  (mostly novel window lengths) measured ~150x this cost versus ~240x for
  repeated lengths. Not mitigated in this beta; would need shape bucketing.
- Arena fragmentation, the IOBinding-on-host pitfall, and GPU engine
  matching are covered in `server/.kb/inference-packaging.md`'s GPU
  engines section; not repeated here.

## fp16 dropped (2026-09-17)

- Naive `onnxruntime.transformers.float16` conversion (`keep_io_types=True`)
  measured slower than fp32 on fresh shapes (Cast nodes around every blocked
  op): streaming 5.6x vs fp32 streaming 9.4x real time.
- Long-window accuracy regresses: 5-minute clip 5.9% WER (fp16) vs 0.9%
  (fp32).
- NeMo 3.0's `autocast` fp16 export yields an invalid graph (mixed LayerNorm
  types).
- ORT's conformer fusion mis-fuses NeMo's relative-position attention.
- A real mixed-precision export (attention scores, Softmax, LayerNorm kept
  fp32) or the TensorRT EP could recover fp16; neither attempted.

## Keyword biasing (int8 CPU, 2026-09-19)

`myna/testbed/keyword_bias.py` plus `_ParakeetOnnx._decode_biased`: each
keyword, as written and capitalised, is spelt in every segmentation the
vocabulary allows; a token extending a match gets `BOOST_PER_CHAR` (3.0) per
keyword character it spells, and only when the unboosted argmax is non-blank.
A boosted match is provisional: if it breaks, or the audio ends inside it,
the decode rewinds to its first boosted token and takes the model's choice.

- The rewind is what makes a long list safe. Without it, 100 keywords on the
  ordinary slice's 82 short clips took WER 1.62% -> 6.28%: one-letter
  word-start pieces (" W", " N") win at every word and never complete
  ("wealth" -> "Wealth", "Inter Inter Inter"). With it, 1.41%.
- Per character beats per token: with the rewind, a flat per-token boost
  needs 10 to reach 3.0-per-character's tune recall (74.8%), at WER 4.13%
  against 2.88% and 7 false hits against 0.
- A second constant capping the total logit deficit a match may borrow cut
  false hits but lost WER at every setting tried; not shipped.
- Tuned on half the frozen set, split by keyword (clips sharing a keyword
  are one component; the component's smallest lowercase keyword's sha256
  parity picks tune or holdout; 142/142 clips). Tune, batch: boost 1 ->
  recall 44.2%, WER 4.32%; 2 -> 62.6%, 3.42%; 3 -> 74.8%, 2.88%, FH 0;
  4 -> 77.9%, 2.74%, FH 2/426; 5 -> 77.3%, 3.22%, FH 8. Holdout keeps
  climbing to 4 (74.4%), but at 4 the 100-keyword list costs the ordinary
  slice WER (2.20%, 7 false hits), so 3.0.
- The stress list is the first 100 of the set's 196 keywords by sha256 hex
  of the keyword, sent to every clip with `bench --keyword-list`; the
  ordinary slice is `manifest-balanced.json` (83 clips, 7 keyword
  occurrences in its references).

| run (bench, rows `final`/`stress*`) | WER off -> on | KW recall off -> on | false hits on |
|---|---|---|---|
| tune, batch | 5.60 -> 2.88 | 16.0 -> 74.8% | 0/426 |
| holdout, batch | 7.13 -> 4.58 | 9.8 -> 59.8% | 1/426 |
| full, batch | 6.36 -> 3.73 | 12.8 -> 67.3% (as written 63.6%) | 1/852 |
| holdout, streaming | 7.16 -> 4.70 | 9.8 -> 58.5% | 1/426 |
| full, streaming | 6.37 -> 3.78 | 12.8 -> 66.7% | 1/852 |
| 100-list on every clip, batch | 6.36 -> 5.41 | 16.7 -> 63.2% | 13/28229 |
| 100-list on every clip, streaming | 6.37 -> 5.35 | 16.7 -> 63.8% | 12/28229 |
| ordinary slice + 100-list, batch | 1.57 -> 1.42 | 2/7 -> 6/7 | 1/8294 |
| ordinary slice + 100-list, streaming | 1.46 -> 1.30 | 2/7 -> 6/7 | 1/8294 |

Re-measured 2026-10-13 on the shipped upstream int8 encoder (the table above
predates the 2026-09-21 encoder swap), same frozen set and stress list:

| run | WER off -> on | KW recall off -> on | false hits on |
|---|---|---|---|
| full, batch | 6.00 -> 3.63 | 19.0 -> 67.6% | 0.12% |
| full, streaming | 6.05 -> 3.71 | 19.0 -> 67.0% | 0.12% |
| ordinary slice + 100-list, batch | 1.46 -> 1.34 | 3/7 -> 6/7 | 1/8294 |

RTFx 49.7 -> 49.4 batch: no measurable cost. The one stress false hit is
still "interdicted" for "entered".

- Ordinary-slice clips with no listed keyword in the reference (77): WER
  1.25% -> 1.21% batch, 1.13% -> 1.09% streaming.
- Cost: +1.6 to +2.7 ms mean decode per clip over ~150 ms (about 1%), median
  finalize +2 ms; building the automaton for 100 keywords takes ~3 ms.
- Weak spot: keywords with a long common word start. "interdicted" is 8 of
  the 14 stress false hits: " inter" alone spells five characters, so
  "entered" turns into it. As-written recall trails recall because the
  capitalised spelling can surface a lowercase keyword capitalised
  mid-sentence.

## Keyword post-correction and murmure's safeguards (2026-10-13)

`KeywordBias.correct`, run on every biased decode: a word that is a keyword
but for accents and case takes the keyword's spelling (a lowercase keyword
keeps a capital the word was written with); a word whose least likely token
had an unboosted probability under `CORRECT_BELOW_CONFIDENCE` (0.8) becomes
the one keyword strictly closest to it within murmure's reach (none under
five letters, one edit under eight, two from eight). Shipped int8 CPU, batch
unless noted:

| run | recall | WER | false hits |
|---|---|---|---|
| frozen set, bias only | 67.6% | 3.63 | 1/852 |
| frozen set, + correction | 74.3% | 3.36 | 3/852 |
| frozen set, streaming, + correction | 73.7% | 3.44 | 3/852 |
| 100-list on every frozen clip, bias only | 65.5% | 4.95 | 7/28229 |
| 100-list on every frozen clip, + correction | 69.5% | 4.89 | 10/28229 |
| ordinary slice + 100-list, + correction | 7/7 | 1.34 | 2/8294 |

- The gate's curve, frozen recall / false hits: 0.3 67.6% / 3, 0.5 69.4% / 3,
  0.7 73.4% / 3, 0.8 74.3% / 3, 0.9 75.2% / 5, ungated 77.7% / 16. Chosen
  at the English knee; the Spanish tier (below), not used to choose it, has
  0 false hits up to 0.9.
- Two edits on a five-to-seven-letter word (the reach "Minya" for "Myna"
  needs, a miss heard live) gated at 0.3 or 0.15 never fired on either
  corpus: no measured win, not shipped. A phonetic key instead took false
  hits to 90/852 ("that" -> "Tait").
- murmure's decode-time safeguards cost recall here, because the rewind
  already stops the runaway matches they guard against. Top-K gate (boost a
  token only inside the raw top 5, top 20 three tokens in): frozen recall
  67.6 -> 61.2%, WER 3.63 -> 3.96, 100-list false hits 7 -> 4. Boost scaled
  by list size (`3.0 - log10(words / 5)`, floor 1): 100-list recall 65.5 ->
  52.9%, WER 4.95 -> 5.14, false hits 7 -> 1. Same direction on Spanish.
- Folding accents into the automaton has no scope: of 105 Spanish and 100
  English keywords only "Erdoğan" and "Chişinău" use a letter the
  vocabulary lacks, and every Portuguese letter is in it.

## A match the model starts itself rewinds too (2026-10-29)

"Myna" sent alone, 72 Kokoro clips (9 voices, 8 sentences, the word as
/ˈmaɪnə/), recall 0/72 off and 6/72 on. The decoder picked a lone " M",
which as the keyword's first piece also took its bonus and outranked " My";
the next frame went "ina", "yn" could not catch up, and no rewind existed for
a match the model chose. Now such a match rewinds once on breaking, and the
frame is decided again among the other pieces against the model's
unboosted choice (`_decode_sequence`, `rerank(rival=True)`):

| run | recall | WER | false hits |
|---|---|---|---|
| Myna clips, batch | 6 -> 21/72 | 14.40 -> 11.32 | - |
| Myna clips, streaming (snap settings) | 6 -> 21/72 | - | - |
| frozen set, batch | 74.3 -> 78.0% | 3.36 -> 3.19 | 3 -> 4/852 |
| frozen set, streaming (snap settings) | 74.0 -> 77.7% | 3.39 -> 3.22 | 3 -> 4/852 |
| ordinary slice + 100-list | 7/7 = | 1.42 = | 2/8294 = |
| Spanish keyword clips | 52.9 -> 53.6% | 6.27 -> 6.21 | 0/597 = |
| Spanish ordinary + 100-list | 3/4 = | 2.83 = | 0/7996 = |

- The new frozen "false hit" is "Newsome" for a spoken "Newsome's" (the
  baseline wrote "Newsom's"); 11 frozen clips gained, none lost. RTF 0.0214
  -> 0.0217.
- What stays missed is mostly homophones: "minor", "miner" and "mine" are
  /ˈmaɪnə(r)/, and the British voice wrote one of them on 7 of 8 clips. No
  acoustic bias can tell those apart. The rest is "Mina": after " My" the
  next frame wants a word end, the vowel already spent.
- Correcting four-letter words one edit away (for "Mina") took the Myna
  clips to 30/72 but the frozen false hits to 7/852 ("eyes" -> "ewes",
  "done" -> "Dene", "like" -> "Lige") for no recall: not shipped, the
  five-letter floor stays.

## A keyword heard as split words (2026-10-29)

Parakeet writes "PulseAudio" as "pulse audio" (and "Pulse Audio"): the right
letters across a word boundary, which neither the automaton (no boundary in
a keyword's spelling) nor a word-at-a-time correction can reach. `correct`
now joins two or three adjacent words whose letters, folded, are exactly a
listed keyword; punctuation may only open the first and close the last.

| run | recall | WER | false hits |
|---|---|---|---|
| PulseAudio/PipeWire clips (36, 6 voices) | 7 -> 45/48 | - | - |
| frozen set, batch | 78.0 -> 78.3% | 3.19 -> 3.16 | 4/852 = |
| ordinary slice + 100-list | 7/7 = | 1.42 = | 2/8294 = |
| Spanish ordinary + 100-list | - | 2.83 = | 0/7996 = |

The 3 left are the French voice's recognition failures ("PLL's audio", a
sentence decoded as French), not split words.

## Spanish keyword tier (FLEURS es_419, 2026-10-13)

`download-corpus --preset fleurs-keywords:es_419` (frozen set
`myna/benchmarker/keywords-fleurs-es_419.json`, mined as its docstring
says): 199 keyword clips with 105 keywords, 80 ordinary clips, and
`stress-keywords.txt`. Sweep with `bench --category keywords
--send-keywords`, and `--category ordinary --send-keywords --keyword-list
.../stress-keywords.txt` for false hits. Parakeet, shipped:

| run | recall | WER | false hits |
|---|---|---|---|
| keywords off | 24.2% | 7.91 | - |
| bias only | 43.6% | 6.79 | 0/597 |
| + correction | 52.9% | 6.27 | 0/597 |
| 100-list on every keyword clip | 53.1% | 6.41 | 1/19635 |
| ordinary slice + 100-list | 3/4 | 2.78 -> 2.83 | 0/7996 |

Recall starts lower than English because the set is mostly foreign names
read in Spanish (Nyiragongo, Addenbrooke, Brzezinski). No Portuguese FLEURS
split is cached; pt_br test is 621 MB.

## Whisper takes no keywords (2026-10-28)

Whisper was given the vocabulary as a prompt line, `"Glossary: <a>, <b>."`:
first in `initial_prompt`, then in faster-whisper's `hotwords`, which it puts
ahead of the previous text in every 30 s window. Neither shipped, and the
adapter's greeting lists no `keywords`: conditioning Whisper on a word list
breaks transcription.

The balanced tier's 5-minute clip, every shipped model with its own snap
policy, the 100-word stress list's 27 that fit 111 tokens, WER off -> on:

| model | batch | streaming |
|---|---|---|
| tiny, int8 | 3.87 -> 177.04% | 4.88 -> 5.60% |
| base, float32 | 3.59 -> 10.47% | 3.44 -> 7.46% |
| small, float32 | 2.01 -> 2.01% | 2.87 -> 2.58% |
| large-v3-turbo, int8 CPU | 1.43 -> 20.52% | 1.87 -> 24.10% |
| large-v3, int8 CPU | 59.97 -> 2.44% | 1.58 -> 3.44% |

The large models ship float16 on the GPU; int8 on the CPU stands in for
them here, and large-v3 batch loops without any glossary at all in it.

- Skips: base ends its first window after one segment with no closing
  timestamp, so faster-whisper seeks a full 30 s and 54 words go. The
  adapter's coverage check sees the 25.8 s hole, but the nudged re-decodes
  carry the same prompt and fail the same way.
- Loops: tiny inserts 704 words of "might have thought that"; turbo batch
  "George?", turbo streaming "Grosvenor", past the recital filter.
- Neither length nor wording decides it: on the
  clip's first 60 s, base lost 54 of 142 words with a one-keyword glossary,
  10 or 27 keywords, the bare list, or one sentence of the story's names, and
  none with three keywords or a sentence of plain prose. In the first three
  windows no_speech_prob rose from 0.05-0.08 to 0.52-0.93, past the 0.6 the
  silence skip needs: any of them decoded under `_LOG_PROB_THRESHOLD` would
  have been dropped whole as silence.
- Small was clean on this clip; one clip is not evidence it is safe.
- Free of the prompt and measured since: Parakeet's post-correction on
  Whisper's word probabilities works (next section). A bias inside the beam
  search is untried.

## Keyword correction after the decode (2026-11-06)

Three routes that leave the decode alone, so none can loop or skip audio.
All three ship (2026-11-06): Whisper's text correction, `keyword_acoustic`
in the FunASR adapter (Latin keywords there take the text correction on
token peaks), and `_toward_keywords` in Parakeet's. Through the real
servers and `myna-bench`, keywords off -> on, whole sets (both halves):

| server | recall | WER / CER | false hits | ordinary + 100-list WER / CER, false hits |
|---|---|---|---|---|
| parakeet | 19.0 -> 87.2% | 6.00 -> 2.76 | 6/852 | 1.53 -> 1.42, 2/8294 |
| whisper base | 14.7 -> 40.4% | 9.57 -> 8.32 | 2/852 | 4.72 -> 4.76, 1/8294 |
| funasr, English | 17.4 -> 34.6% | 8.45 -> 7.58 | 4/852 | 4.18 -> 4.03, 1/8294 |
| funasr, cmn_hans_cn | 28.9 -> 85.9% | 17.12 -> 15.28 | 0/597 | 6.83 -> 6.92, 0 |
| funasr, ja_jp | 29.9 -> 85.6% | 12.08 -> 10.59 | 2/414 (1 off) | 9.26 -> 9.45, 1 |
| funasr, ko_kr | 25.4 -> 77.9% | 11.69 -> 10.56 | 0/300 | 7.41 -> 7.58, 0 |

The FunASR ordinary drift is SenseVoice's own nondeterminism: two runs
with keywords off differ on 18 of 80 Korean clips, as many as off and on.

All offline: plain `transcribe()` or the adapter's decode internals per clip,
not the adapter's batch path; keyword sets are mined from the measured model's
own misses, so "off" recall is low by construction. Every gate was chosen on
the tune half (split by keyword component, as for Parakeet) and the holdout is
reported. The scratch rig is not in the tree.

### Whisper: text correction on word probabilities

`KeywordBias.correct`, unchanged, on faster-whisper's words, each one token
with its word probability (faster-whisper's mean over the word's tokens, not
Parakeet's minimum). Needs `word_timestamps`: +0.7 to +5.3% decode time in
batch; the streaming path already aligns. Frozen English set, gate 0.8, each
model's shipped batch policy:

| model | recall off -> on | WER off -> on | false hits | ordinary + 100-list WER, false hits |
|---|---|---|---|---|
| tiny int8 | 11.0 -> 33.3% | 12.36 -> 11.22 | 4/852 | 6.18 -> 6.06, 0 |
| base fp32 | 14.7 -> 40.4% | 9.60 -> 8.31 | 2/852 | 4.79 -> 4.72, 1/8294 |
| small fp32 | 22.3 -> 46.2% | 7.85 -> 6.63 | 1/852 | 3.30 -> 3.26, 0 |
| large-v3-turbo int8 CPU | 41.3 -> 48.6% | 5.31 -> 4.94 | 1/852 | 2.30 -> 2.26, 0 |

Parakeet's own correction without its bias, for scale: 19.0 -> 52.3%.

### SenseVoice: score the keyword against the audio (CTC)

No pronunciation dictionary. Every run of the greedy tokens holding n-1 to
n+1 letters of an n-letter keyword (letters, not tokens: Korean pieces are
multi-syllable, " 단계") is a candidate; its audio is the frames between the
neighbouring tokens. The keyword replaces the run when its CTC
log-likelihood there is within 3 nats per keyword letter of the run's own.
Best margin first, no overlaps. A Hangul syllable outside the vocabulary
(셴, 룽, 웡) is scored as each in-vocabulary syllable one jamo from it.
The `fleurs-keywords:<locale>` tiers, shipped int8, language auto, withitn;
holdout:

| locale | recall off -> on | CER off -> on | false hits | ordinary + 100-list CER, false hits |
|---|---|---|---|---|
| cmn_hans_cn | 27.7 -> 80.7% | 13.62 -> 11.69 | 0/246 | 5.36 -> 5.33, 0/7839 |
| ko_kr | 18.5 -> 75.9% | 10.92 -> 9.42 | 0/141 | 7.09 -> 7.09, 0/6160 |
| ja_jp | 31.6 -> 84.2% | 8.73 -> 7.67 | 0/165 | 6.46 -> 6.51, 2/6718 (1 off) |

- The margin must scale with keyword length. A flat 6-8 nats let a
  three-syllable keyword replace unrelated words while one-syllable misses
  in long keywords stayed out of reach.
- A confidence gate on top only costs recall: the false hits it would stop
  sit on runs the model was unsure of anyway.
- 4 nats per letter is where ordinary false hits start (1, 2 and 6).
- Cost, ~100 keywords, numpy, one thread: 4.5 (zh, ja) to 9.3 (ko) ms per
  audio second. Exact pruning makes it cheap: the keyword's probability over
  a window is at most the product of each of its tokens' summed probability
  there, and the run's own is at least its greedy path's, so over 99.9% of
  windows are rejected from two cumulative sums; results were identical
  unpruned.
- Pinyin (pypinyin) matching reached 73.9% holdout on Chinese and was
  dropped for this. Its false hits were all rare readings of a heteronym
  (其 as ji, 埃 as zhi), cured only by reading both sides in context, and
  it needs a dictionary per language (jyutping for Cantonese, a morphological
  analyser for Japanese).

### Parakeet: the same scoring through TDT

The decoder_joint export takes `targets` [B, U] and returns the whole
[frames x labels] grid, so a keyword over a window is one call plus a TDT
forward in numpy (token and duration heads; a blank moves 1+ frames).
Candidates come from a loose spelling prefilter (one word within ~40% edits;
several words only when they spell the keyword split, at most one edit), never
a run already holding a keyword. Then `correct` runs as shipped. Frozen set,
holdout:

| pipeline | recall | WER | false hits | ordinary + 100-list WER, false hits |
|---|---|---|---|---|
| bias + correct (shipped) | 73.8% | 3.72 | 3/426 | 1.38, 2/8294 |
| bias + acoustic 2.0/letter + correct | 85.4% | 3.18 | 5/426 | 1.38, 2/8294 |
| acoustic + correct, no bias | 75.0% | 3.75 | 4/426 | 1.34, 1/8294 |

- Without the two candidate rules WER rose: a short word beside a keyword
  ("to", "a", "Woodley, who") costs too little to keep, and a correct
  keyword was swapped for a listed neighbour (Morrel -> Morven).
- The acoustic pass alone misses some of what `correct` fixes
  (Dorriforth, portmanteau): chain them.
- Cost 5 ms per audio second with a clip's five words, 22 ms with the
  100-list; the joint calls per spelling are not batched yet.

## Nemotron 3.5 ASR (September evaluation)

`nvidia/nemotron-3.5-asr-streaming-0.6b`, rev `ea30d66`, OpenMDW-1.1.
Evaluated 2026-09-17, never packaged as a snap engine.

Export recipe (NeMo 3.0.0, inference is torch-free):

- `model.set_export_config({"cache_support": "True"})` before
  `model.export()` yields a cache-aware encoder (cache I/O ports) and
  decoder_joint.
- NeMo's exporter drops the language-prompt projection
  (`PromptStreamingMixin._apply_prompt_to_encoded`); export it separately as
  its own small `torch.onnx.export` graph (`prompt-model.onnx`).
- torch's ONNX exporter cannot export NeMo's STFT, so the mel frontend
  (`normalize` NA) runs as hand-written numpy at inference instead of an
  exported graph.
- The exported encoder bakes in `drop_extra_pre_encoded=2` and
  `valid_out_len`; the inference loop follows NeMo's
  `pad_and_drop_preencoded` semantics and flushes with one chunk of silence
  appended after the real audio.
- Recipe script: revived 2026-11-02 as `dev/nemotron/export_nemotron_onnx.py`
  (both checkpoints), see the next section.

Accuracy, 2026-09-17:

| corpus | Parakeet v3 (fp32 CUDA) | Nemotron 3.5 |
|---|---|---|
| English, 82 clips | - | NeMo 2.77% (offline 2.67), ONNX fp32 2.57%, int8 2.93% |
| FLEURS de | 5.2 | 10.6 |
| FLEURS es | 3.1 | 5.3 |
| FLEURS fr | 5.7 | 11.4 |
| FLEURS ar | not covered | 13.7 (CER 3.9) |
| FLEURS ja | not covered | CER 15.8 |
| FLEURS zh | not covered | CER 25.9 |

WER unless marked CER, 40-clip subset, scored with the current normalizer
(typographic apostrophes folded, see `server/.kb/benchmarking.md`). Before
that fold, fr read 8.1/13.7: FLEURS French references use U+2019 for
elisions, so an ASCII hypothesis like "L'accident" split into two words
against them. Nemotron's de/es/fr still run 26-28% above NVIDIA's published
figures (8.31/4.11/9.03); not root-caused. Parakeet's full-FLEURS numbers
(862/908/676 clips) land within bootstrap CI of NVIDIA's published figures -
see `server/.kb/benchmarking.md`.

- CPU int8 real-time headroom: 14.9x on 4 threads (fp32 CPU 8.4x), fixed
  1.12 s chunk shapes so no CUDA-style per-shape cost applies.
- Why parked: ~2x worse WER than Parakeet where Parakeet has the language.
  Nemotron's advantages are native streaming (~1.1 s chunk-to-commit, no
  VAD-driven windowing) and language coverage (ar/ja/zh/hi/ko, which
  Parakeet v3 lacks).
- If revived: restore the recipe script, lock it (`uv lock --script`) with
  the fsspec override `dev/parakeet/export_parakeet_onnx.py` carries, and
  bring the scratch scripts (streaming inference loop, int8 quantization,
  FLEURS harness, NeMo reference runner) beside it, each with its own PEP 723
  metadata; never add nemo/torch to `server/uv.lock`.

## Nemotron streaming ONNX adapter (2026-11-02)

`myna.testbed.nemotron` (`--adapter nemotron`) runs
`nvidia/nemotron-speech-streaming-en-0.6b` cache-aware, torch-free, for live
captions. Built from the captions feasibility run, which found it the only
method that commits every word within about a second and never rewrites one.

- Export: `dev/nemotron/export_nemotron_onnx.py MODEL.nemo OUT 6` (right
  context 6 = 0.56 s chunks; 13 = 1.12 s gave the same WER, commits 0.4 s
  later). The graph bakes in `drop_extra_pre_encoded`, so every step, the
  first too, follows NeMo's `pad_and_drop_preencoded`: 56 new mel frames
  after 9 already seen, zeros before the first. NeMo's caches are
  layer-first, the export's batch-first, and the cache time axes are
  dynamic, so the export writes their sizes to `config.json`.
- Mel: Parakeet's `nemo128.onnx` normalises per feature; Nemotron is
  `normalize: NA`. `dev/nemotron/raw_mel.py` cuts that graph before its
  normalisation tail: mean abs diff 0.0008 vs NeMo (the 16.6 maximum is
  NeMo's `log(2^-24)` floor in digital silence), WER unchanged.
- Parity (`dev/nemotron/parity.py`, NeMo and the export stepped through the
  same chunks): fp32 encoder within 7e-6 and token-identical over whole 5-6
  minute clips. The adapter fed in 100 ms pieces reproduces the harness's
  int8 transcript exactly, which pins the incremental mel and the chunking.

Caption clips, real time over the wire (the captions feasibility harness, not
on main: `dev/captions_eval` on branch `captions-live`), int8, 2
encoder threads:

| clip | method | WER | commit p50/p90 | server CPU |
|---|---|---|---|---|
| nogaps | torch dynamic int8 (Linear only) | 3.7% | 0.3 / 0.7 s | 0.51 |
| nogaps | ONNX int8 (this adapter) | 3.7% | 0.3 / 0.7 s | 0.36 |
| alestro | torch dynamic int8 (Linear only) | 11.3% | 0.4 / 0.7 s | 0.50 |
| alestro | ONNX int8 (this adapter) | 12.2% | 0.4 / 0.7 s | 0.34 |

The ONNX rows quantised every op. ORT's `quantize_dynamic` takes the
encoder's 77 Conv layers as well as its 289 MatMuls; torch's dynamic int8 took
the Linear layers alone. Quantising the encoder's MatMuls only (what the export
now does; the decoder_joint keeps every op) scored offline through the
adapter, on chug (Ryzen 9 7950X3D; its all-ops transcripts are byte-identical
to the laptop's):

| encoder | nogaps | alestro | CPU, 2 threads | size |
|---|---|---|---|---|
| fp32 | 3.5% | 11.9% | 0.48 | 2.4 GB |
| int8, every op | 3.7% | 12.2% | 0.24 | 622 MB |
| int8, MatMul only | 3.8% | 11.9% | 0.205 | 839 MB |

The WER differences are a few words in ~1900, inside the noise of two clips;
the CPU difference is not (the int8 Conv kernels run slower than fp32 ones).
Exporting and quantising peak around 7 GB: cap the job, do not lift the cap,
or use a throwaway machine.

The 3.5 multilingual checkpoint goes through the same export (its
`prompt-model.onnx` within 3e-5 of torch; left context 56, not 70) and the
same adapter, MatMul-only int8, on chug:

| clip | language | 3.5 WER | English model | torch 3.5 int8 |
|---|---|---|---|---|
| nogaps_fr | fr | 15.5% | - | 15.5% at 0.65 core |
| nogaps_fr | auto | 15.4% | - | - |
| nogaps | en | 4.8% | 3.8% | - |
| alestro | en | 12.5% | 11.9% | - |

0.20 core throughout. English stays on the English model. The language tags
3.5 writes after terminal punctuation are single vocabulary pieces carrying
their own word-start space; the adapter drops both.

