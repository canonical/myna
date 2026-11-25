# Preface

What Myna measured about post-ASR text processing (tidy, numbers, addresses, commands, casing, emoji, translation) when choosing between rules, small tagger models and small language models: the corpora, the metrics, the verdict per approach and per operation, the overfitting finding, the caveats, which freely redistributable assets are usable, and how to rerun it. Read it before proposing a model for a text step or changing the evaluation corpora. The experiment ran overnight on 2026-11-24/25; the decisions that followed are processing-graph DECISIONS 10.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

## Verdict

1. **Few-shot small models (0.27-1.7B) lose.** They trail the rules or the shipped tagger everywhere and alter 5-40% of clean sentences. Only Qwen3-4B few-shot is good (tidy 88-93, numbers 81-89, run 1) at 2-3.5 s, outside the budget.
2. **Fine-tuned (LoRA) Qwen3 wins on accuracy** for tidy, numbers, addresses and non-English commands, but no fine-tuned arm passes the break gate (0-6% of clean sentences altered), the numbers arms change digits (6-19 per 1000) and they cost 150-800 ms where rules cost microseconds.
3. **What passes the gates and is fast:**
   - tidy: the word tagger (one 49 MB int8 model for en/fr/de/es, 2-6 ms, damage 0). English ships today as `myna-disfluency-en`; the multilingual one is a follow-up.
   - numbers in fr/de/es: text2num (MIT Rust crate) plus a small convention pass.
   - everything else in English: the rules.
4. **Shipped as a result (DECISIONS 10):** the first release offers Tidy, Translate and Chinese script. Commands, emoji, snippets, casing, addresses, numbers, profanity, typography and Keep the original were removed from the tree and return per language when their research task lands. The rules that were measured are linked [below](#the-deleted-steps).

## Blind results

All numbers are on run 2 (blind, 932 items plus 100 FLEURS dev clean sentences per language), unless marked run 1. `acc` = share of corpus items exactly right, `brk` = share of clean sentences altered, `dmg` = per 1000 items wrong and unrecoverable. Higher acc, lower brk and dmg are better.

| op | lang | rules (removed) | deterministic or tagger best | LoRA 0.6B best | LoRA 1.7B | few-shot Qwen3-0.6B |
|---|---|---|---|---|---|---|
| tidy | en | 73.0 (tagger 82.5) | rule+multi-tagger 87.3, brk 1, dmg 0 | 88.9 brk 5 | **95.2** brk 1 | 71.4 brk 22 |
| tidy | fr | 35.0 | multi-tagger 81.7, brk 0, dmg 0 | 81.7 brk 0 | **85.0** brk 0 | 61.7 |
| tidy | de | 35.0 | multi-tagger 78.3, brk 2 | 76.7 | **86.7** brk 4 | 46.7 |
| tidy | es | 35.0 | multi-tagger 76.7, brk 0 | 80.0 | **86.7** brk 0 | 61.7 |
| numbers | en | 74.1 brk 5 | text2num+conv 72.4 | 91.4 brk 6 dmg 6 | **94.8** brk 2 | 75.9 brk 21 |
| numbers | fr | 33.9 | text2num+conv 78.0 brk 7 | 89.8 dmg 19 | **91.5** | 52.5 |
| numbers | de | 32.8 | text2num+conv 77.6 brk 7 | **93.1** brk 6 | 93.1 | 44.8 |
| numbers | es | 33.9 | text2num+conv 76.3 brk 6 | **89.8** | 89.8 | 47.5 |
| addresses | en | 73.7 | NeMo ITN 35.1 | **96.5** brk 3 | - | 64.9 |
| addresses | fr/de/es | 37.5-38.6 | NeMo ITN 26-36 | **93.0-96.5** brk 2-4 | - | 48-51 |
| commands | en | **100** (the corpus follows the rules) | - | 98.2 | - | 43.9 |
| commands | fr/de/es | 38.6 (no rule) | - | **93.0-98.2** brk 0-1 | - | 37-46 |

Every dAcc against the rules has a paired bootstrap 95% interval in the full report (`file:///home/charles/myna-processing-graphs/bench/smallmodel/results/run2-report.md`). The LoRA and text2num gains over the non-English rules exclude zero; text2num+conv2 against the English numbers rule is +0.0 [-8.6, +8.6], a tie.

Latency (4 threads, chug's Ryzen 7950X3D):

| arm | quiet box (load about 5) | box at load about 26 |
|---|---|---|
| LoRA 0.6B | p50 150-200 ms, p95 215-250 ms | - |
| LoRA 1.7B | - | p50 565-780 ms, p95 850-1030 ms |
| Qwen3-4B few-shot | - | p50 1.2-3.7 s |
| taggers (1 thread) | p50 2-3 ms, p95 6-8 ms | - |
| rules, text2num | microseconds | - |

No laptop-class measurement exists yet.

## Recommendation per operation

| op | tier 1 (default, fast, gated) | tier 2 (optional, measured) | why |
|---|---|---|---|
| tidy | multilingual tagger, one model for en/fr/de/es | LoRA 0.6B/1.7B behind ConservativeEdit, once the laptop p95 is known | Tagger damage is 0 by construction at 2-6 ms. Next: add phrase restarts and "actually" repairs to the generator, retrain, rescore on run 2. |
| numbers | en: rules; fr/de/es: text2num plus a per-language convention pass (it/pt/nl untested) | none until a digit-multiset verifier rejects LoRA's digit slips | Digit changes are the unrecoverable class. |
| addresses | per-language word tables (point/arobase, Punkt/at, punto/arroba) in a closed grammar | LoRA as a challenger behind an address verifier | LoRA's 93-96% shows the task is learnable; the deterministic grammar was only tried in English. |
| commands | per-language phrase tables in a closed grammar | none | Destructive commands stay deterministic; LoRA at 93-98% shows the phrase sets are small and regular. |
| casing, emoji | rules; emoji names per language from CLDR | none | Models fail the negatives (run 1: casing rules 72.6, models 18-33; emoji rules 91.0, models 4-35). |

A generative tier stays worth keeping for content-understanding commands ("format as a list", "make it shorter"); this benchmark did not test those.

## What the blind re-score showed

1. **Everyone drops on tidy, 12-17 points**, the English rules included (-6.5). Run 2 tidy is harder: it holds phrase-level restarts ("Send it to Anna, send it to Anna by noon") and "actually"/"plutôt" repairs. Taggers and LoRA miss the same items, so the gap is in the synthetic training generator, not in the model class. Three to six misses per language are comma-only differences.
2. **Tuning on run 1 failures overfitted.** LoRA tidy v3's lead over v1 shrank from +10.6 (run 1) to +4.1 (run 2): about 4 points of real gain, 6 fitted to run 1's failures. German tidy v3 fell 19 points (95.9 to 76.7). The never-tuned English tagger fell 12.2, so part of the drop is only the harder corpus.
3. **text2num stays flat (-0.1)**: a deterministic arm generalises. Its break rose to 6-8% on FLEURS dev because it writes legitimately spelled numbers of 10 and over as digits; that may not be a real break and needs reading.
4. **LoRA tidy leaks other tasks.** The multi-task 1.7B turned "huit cents euros" into "800 €" inside tidy, and v3 paraphrased "Pouvez-vous" to "Pourriez-vous".
5. **Run 1 break figures were optimistic** for the LoRA numbers arms (3-6 on the fresh clean set against 0-3) and for text2num.

Cross-run table, run 1 against run 2 for every arm and cell: `file:///home/charles/myna-processing-graphs/bench/smallmodel/results/run2-crossrun.md`.

# Important

## Caveats

1. **Same author for everything.** Every corpus and every training set was written by Claude agents, so they share blind spots; no native speaker has checked fr/de/es. The recorder archive (real speech) is the missing ground truth.
2. **Break-rate resolution.** 100 clean sentences per language cannot resolve the 0.5% gate: one sentence is 1%. It needs 1000 or more.
3. **Latency was measured on a shared box**, not a laptop CPU class; the 4B and 1.7B figures were taken at load about 26.
4. **Rule-shaped references.** The English commands, casing and emoji references were checked against the Rust output, which favours the rules (commands en 100%).
5. **Run 1 is not blind.** The LoRA iterations 2 and 3 and the tagger were informed by run 1's per-class failures. Quote run 2 for tuned arms. Run 1 clean sentences (first 100 lines of the repository's FLEURS test slices) were never in any training data, but lines 100-199 are.
6. **Each operation is graded on its own copy of its language's 100 clean sentences**, so breaks are comparable across operations but not independent samples.
7. **Three English inputs occur in both runs** ("I told him to scratch that idea.", "The deadline is March twenty first.", "The conference runs from May twelfth to May fourteenth."), written independently by both authors.
8. **The ConservativeEdit verifier is not applied** to the tagger rows; the hybrid rows model it with a letters-subsequence check (changes scores by 1.6 points at most). The Rust rule was graded through a bridge that is not in the repository, so the harness cannot rerun it; 
9. **Per-language taggers (run 1 only, not blind):** fr 93.4, de 94.3, es 88.0 against 94.3, 95.1 and 92.0 for the one shared model, with 150k examples each against 600k. The shared model also wins on cost (one file), so ship one multilingual tagger.
10. **Agent-authored corpora of this size say little about real speech.** Treat every figure here as a relative ranking of arms, not as an accuracy users will see.

## Rules for changing this

- A change to a file under `textcorpus/eval/` is a change to the benchmark: regenerate `textcorpus/eval/SHA256SUMS` with `text_corpus.write_eval_sums()` and record why in the commit body. Never edit run 2 to make an arm pass; add a run 3 instead.
- Report run 2 (blind) numbers, with the break and damage columns, for any model arm; accuracy alone hides a model that alters clean text.
- Quote a latency only with the load of the machine it ran on.

# Architecture

## Corpora

Both live in `server/src/myna/benchmarker/textcorpus/eval/` and are pinned by `eval/SHA256SUMS`; provenance and licences are in `textcorpus/NOTICE`. `text_corpus.load_eval(run)` builds the items.

| | run 1 | run 2 (blind) |
|---|---|---|
| items | 3521 | 2532 |
| operations | tidy, numbers, addresses, commands in en/fr/de/es; casing, emoji in en | tidy, numbers, addresses, commands in en/fr/de/es |
| written | 2026-11-24 evening, with the arms | 2026-11-25 04:11-04:16, after every arm was frozen, from the stated conventions only |
| clean sentences | first 100 lines per language of `clean_en` and `translate_en_<l>` (FLEURS test, CC-BY-4.0) | 100 FLEURS dev sentences per language, none in any arm's training data |
| negatives | about 35% hard negatives (text that must stay) | about 35% |

An item is `(id, op, lang, input, refs, kind, class)`; `refs` lists every output that counts as right, `kind` is `positive`, `negative` or `clean`. The clean sentences of run 2 were checked (letters only, case and accents folded, equality and substring) against every training and dev text of every arm and against the repository's 200 FLEURS test lines: no hits.

## Metrics

Ported from the experiment's `score.py` into `server/src/myna/benchmarker/text_eval.py`.

- **acc**: output equals a reference after normalisation (NFC, one kind of space and quote, no emoji variation selector, whitespace collapsed per line). Case and punctuation count.
- **break**: share of clean sentences whose normalised output differs from the input.
- **damage**: per 1000 items, wrong and unrecoverable: the output's letters are not a subsequence of the input's (an invented word or changed digit) or a tidy kept under half the letters.
- **dAcc**: accuracy minus the baseline arm's, with a paired bootstrap 95% interval (2000 resamples, seeded).
- **hybrid** `hyb:<arm>`: the arm's output when a verifier passes (non-empty, letters a subsequence of the input's, no new line breaks unless the operation is `commands`, a tidy keeps half the letters), else the baseline's. A hybrid took damage to 0 at the same accuracy for the tuned arms and lifted only the few-shot ones (Qwen3-0.6B addresses en 64.9 to 70.2).
- The ship gates of `myna-bench text` (UER at most 1%, edit reduction, polarity, chrF, p95) are separate and unchanged; see `text_suite.py`.

The 212 rows of the run 2 report reproduce byte for byte from the raw outputs with the ported scorer.

## Arms

| arm | what | status in the tree |
|---|---|---|
| rules | the Rust rule steps, driven through a bridge crate | removed (see below); tidy's English rule `disfluency.rs` stays |
| tagger | `myna-disfluency-en`, MiniLM-L12 word tagger, int8 ONNX, tau 0.9 | ships; `text/tagger.py` |
| tagger-multi | one 49 MB int8 tagger for en/fr/de/es (600k examples) | not shipped; recipe in the workstream |
| text2num (+conv) | MIT Rust crate, threshold 10, a per-language post-pass for currency, percent and clock time | not shipped |
| NeMo ITN | NeMo-text-processing WFST grammars (Apache-2.0), Python and pynini (413 MB stack) | measured only; grammars 15.7 MB for four languages |
| LoRA | Qwen3 0.6B or 1.7B, one adapter per operation (tidy, numbers, addresses, commands) trained on synthetic data | not shipped; recipe in the workstream |
| few-shot | Qwen3 0.6B/1.7B/4B, Gemma 3 270M/1B, three worked examples per operation and language | measured only |

Training recipes, the data generators and the leakage checks stay outside the repository:

- LoRA: `file:///home/charles/myna-processing-graphs/bench/smallmodel/lora/`
- multilingual tagger: `file:///home/charles/myna-processing-graphs/bench/smallmodel/tagger_multi/`
- text2num: `file:///home/charles/myna-processing-graphs/bench/smallmodel/text2num/`
- NeMo ITN: `file:///home/charles/myna-processing-graphs/bench/smallmodel/nemo_itn/`
- the original run script, prompts and the hour-by-hour log: `file:///home/charles/myna-processing-graphs/bench/smallmodel/` (`TIMELINE.md`)

The raw per-item outputs of both runs (52 MB) were on `chug:/var/tmp/smallmodel/run1` and `run2`; they are scratch space. Put them in `--results` directories to rescore them.

## The deleted steps

The steps below were measured as the rules arm, then deleted from the tree by DECISIONS 10. The links are pinned to `3d211c64` (`origin/processing-graph` before the deletion).

| step | rule measured | blind acc, rules | verdict |
|---|---|---|---|
| numbers | [`numbers.rs`](https://github.com/canonical/myna/blob/3d211c64a931e5e34a4676771ab94404ae8af15f/client/myna-desktop/src/process/numbers.rs#L174) (`apply`) | en 74.1 (brk 5); fr/de/es 33-34, there is no rule | English stays rules; fr/de/es text2num+conv 76-78 |
| addresses | [`addresses.rs`](https://github.com/canonical/myna/blob/3d211c64a931e5e34a4676771ab94404ae8af15f/client/myna-desktop/src/process/addresses.rs#L87) | en 73.7; elsewhere 37.5-38.6 | per-language word tables in a closed grammar; NeMo ITN fails here (35.1 en) and breaks 3-16% |
| commands | [`commands.rs`](https://github.com/canonical/myna/blob/3d211c64a931e5e34a4676771ab94404ae8af15f/client/myna-desktop/src/process/commands.rs#L152) | en 100 (defines the corpus); elsewhere 38.6 | per-language phrase tables; no model |
| casing | [`casing.rs`](https://github.com/canonical/myna/blob/3d211c64a931e5e34a4676771ab94404ae8af15f/client/myna-desktop/src/process/casing.rs#L76) | run 1 only: 72.6 | rules win; every model altered 7-73% of clean sentences |
| emoji | [`emoji.rs`](https://github.com/canonical/myna/blob/3d211c64a931e5e34a4676771ab94404ae8af15f/client/myna-desktop/src/process/emoji.rs#L51) | run 1 only: 91.0 | rules win; models failed the negatives |
| snippets | [`snippets.rs`](https://github.com/canonical/myna/blob/3d211c64a931e5e34a4676771ab94404ae8af15f/client/myna-desktop/src/process/snippets.rs#L49) | not measured | deterministic by nature |
| profanity | [`profanity.rs`](https://github.com/canonical/myna/blob/3d211c64a931e5e34a4676771ab94404ae8af15f/client/myna-desktop/src/process/profanity.rs#L28) | not measured | lists (LDNOOBW, CC-BY-4.0) |
| typography | [`typography.rs`](https://github.com/canonical/myna/blob/3d211c64a931e5e34a4676771ab94404ae8af15f/client/myna-desktop/src/process/typography.rs#L55) | not measured | deterministic by nature |
| tidy (English rule) | [`disfluency.rs`](https://github.com/canonical/myna/blob/3d211c64a931e5e34a4676771ab94404ae8af15f/client/myna-desktop/src/process/disfluency.rs#L170) | run 2: 73.0 alone, 87.3 with the tagger | stays; `rules-lang`, a per-language rule in front of the multilingual tagger, hurt fr/de/es by about 10 points |

Keep the original has no corpus: it is a switch on which text is committed, not a transformation.

## Approaches, from the literature

Research of 2026-11-24, nothing measured; each claim carries its source in `file:///home/charles/myna-processing-graphs/design/processing-approaches-research.md`. What survived the measurements:

1. No product publishes enough to say an LLM beats rules on average. Commercial dictation cleanup runs in the cloud or on a 3B-class model, and none publishes accuracy against a rule baseline.
2. The rigorous normalisation comparison is NVIDIA's GPT study (https://arxiv.org/pdf/2309.13426): GPT-4 made about 40% fewer errors than a WFST on English text normalisation but 9 unrecoverable errors against the WFST's 1 in 7551 sentences. Tagger formulations exist because they cannot hallucinate.
3. Production ITN is still per-language WFST grammars (NeMo, Kestrel), with neural taggers added where the grammar is weak. NeMo has ITN directories for 18 languages; English alone is complete. Our measurement agrees: NeMo wins fr/de/es numbers over no rules and loses English to Myna's rules.
4. Spoken editing commands are rule or grammar based in every mature offline system (Dragon, Talon, Apple Voice Control, Windows). LLM "scratch that" handling is a cloud-product claim. Our measurement agrees: destructive commands stay deterministic.
5. Text-only LLM correctors broke 13 to 32% of already-correct transcripts in a 2026 study (https://arxiv.org/abs/2610.30385). That is why the evaluation carries a break rate and a verifier.
6. No source reported a Qwen3-0.6B, Gemma 3 270M or SmolLM2 result for ITN, disfluency or commands before this experiment.

Design rule that follows: a model for open-ended or context-dependent steps (disfluency repair, punctuation and case, rewrite), a deterministic closed grammar for anything with a safety role (commands, code casing, URLs and emails, emoji, snippets, profanity, script), both behind one per-language interface with a verifier.

## Translation

Translate is graded by the existing `translate_en_<l>` suites and needs nothing from `--eval`. Measured on the 200 FLEURS `en_us -> fr_fr` gate (chrF at least 0.58, warm p95 under 400 ms), the shipped runtime, Bergamot (MPL-2.0, built from source without MKL), gives:

| model | chrF | warm p50 / p95 | RSS | xz size |
|---|---|---|---|---|
| Bergamot base-memory, beam 1 | 0.701 | 10.9 / 18.8 ms | 279 MB | 24.4 MB |
| Bergamot tiny, beam 1 | 0.698 | - / 10.5 ms | 223 MB | 14.9 MB |
| opus-mt-en-fr int8 via CTranslate2 | 0.69-0.70 | - / 274-371 ms | - | 61 MB |
| opus-mt-en-fr int8 via ONNX (optimum) | - | - / 588 ms | 825 MB | - |

Full table: `file:///home/charles/myna-plan/tasks/bench-clean-translation-models.md`.

## Distributable assets

A survey of 2026-11-24 of freely redistributable models, rule sets and data for the text steps, from Hugging Face, GitHub and NGC metadata (`file:///home/charles/myna-processing-graphs/design/distributable-text-assets.md`, every row tagged verified, inferred or measured). A permissive tag on a model does not clear its training data; where the data is encumbered the row says so.

### Usable

| asset | for | licence | note |
|---|---|---|---|
| [Horizon-Labs multilingual-punctuation-truecase-small](https://huggingface.co/Horizon-Labs/multilingual-punctuation-truecase-small) | punctuation and truecase, 88 languages | Apache-2.0; data ODC-BY | int8 ONNX 268 MB; 20-45 ms on a loaded box; released 2026-11-13, no published eval, needs our own gate; keeps fillers |
| [NeMo-text-processing](https://github.com/NVIDIA/NeMo-text-processing) grammars and TSV tables | ITN rule source | Apache-2.0 | grammars and tables only; its pretrained NGC checkpoints are not OSI-licensed |
| [WeTextProcessing](https://github.com/wenet-e2e/WeTextProcessing) | zh and ja ITN | Apache-2.0 | |
| [text2num](https://github.com/allo-media/text2num) (fr en es pt de nl it), [number-parser](https://github.com/scrapinghub/number-parser) | spoken numbers | MIT, BSD-3 | text2num is what the blind numbers arm used |
| Qwen3 0.6B/1.7B, Qwen3.5 0.8B/2B, Granite 4.0 nano, Gemma 4 E2B | fine-tune bases for rewrite and commands | Apache-2.0 | Gemma 4 E2B is over a 1 s CPU budget; check the licence text before relying on it |
| [Unicode CLDR annotations](https://github.com/unicode-org/cldr), [Emojibase](https://github.com/milesj/emojibase) | emoji names per language | Unicode License v3, MIT | names are written for search, not for speaking; the Unicode notice travels with the data |
| [LDNOOBW](https://github.com/LDNOOBW/List-of-Dirty-Naughty-Obscene-and-Otherwise-Bad-Words) | profanity lists, 28 languages | CC-BY-4.0 | attribution in the snap; not severity-graded |
| [OpenCC](https://github.com/BYVoid/OpenCC) | Chinese script | Apache-2.0 | shipped |
| [OPUS-MT](https://huggingface.co/Helsinki-NLP/opus-mt-en-fr), opus-mt-tc-big, M2M100, MADLAD-400 3B | translation beyond Bergamot | Apache-2.0, CC-BY-4.0, MIT, Apache-2.0 | OPUS corpora are mixed-licence; MADLAD is heavy |
| [Talon community](https://github.com/talonhub/community), [Cursorless](https://github.com/cursorless-dev/cursorless) | command and formatter vocabularies | MIT | grammars to read, Talon itself is closed |
| [disfl_qa](https://huggingface.co/datasets/google-research-datasets/disfl_qa), [aawaaz cleanup set](https://huggingface.co/datasets/shantanugoel/aawaaz-transcript-cleanup-dataset) | English disfluency and cleanup pairs | CC-BY-4.0, MIT | questions only, and generator terms unchecked, respectively |
| [Herald cleanup 1.7B](https://huggingface.co/herald-dictation/herald-cleanup-1.7B-GGUF) | recipe reference | Apache-2.0 | cleanest stated provenance of the cleanup models; no metrics |

### Unusable

| asset | why |
|---|---|
| NLLB-200, SeamlessM4T v2, Aya Expanse, xLAM-2, Hammer2.1, CoEdIT weights | CC-BY-NC-4.0 |
| `stillerman/fdt-disfluency-*` (all) and its synthetic set | Apache tag, but self-declared research artefact over CC-BY-NC-SA DailyDialog data; the 4-tag recipe is sound, so retrain on clean data |
| `datasets-CNRS/paroles-disfluentes`, several T5 grammar models | CC-BY-NC-SA-4.0 |
| DISCO (en/hi/de/fr disfluency) | no licence file, all rights reserved; ask the authors |
| MTUOC-Aina truecasing, `sarvam-translate` | GPL-3.0 on weights |
| `num2words`, LanguageTool | LGPL, embed carefully |
| Glimpse C1, FUTO keyboard, `mandododo/polyglot-cleanup-*` | product-bound or non-commercial terms, Gemma flow-down |
| NeMo NGC checkpoints (`punctuation_en_*`, `itn_en_*`) | NGC Terms of Use, not OSI |

Custom terms that need a legal decision, not treated as free: Gemma 3 and FunctionGemma and TranslateGemma (Gemma Terms and Prohibited Use Policy), Llama 3.2 (700M-MAU clause, naming), LFM2 (commercial use only under USD 10M revenue, effectively unusable for redistribution at our scale).

### Still open

1. No open multilingual disfluency data with a licence. Options: ask the DISCO authors, synthesise from licence-clean text with an injector and a hand-written per-language filler list, or mine CC0 and CC-BY transcripts (AMI, VoxPopuli) as Herald did.
2. No open per-language filler lexicon exists; write it by hand per language and validate against a disfluent corpus.
3. Italian ITN: NeMo has no Italian ITN directory; text2num covers it, unmeasured.
4. Horizon needs its own F1 and WER on Myna's passages per language, a latency measurement on an idle laptop-class CPU, and a check on elided forms (l', j') and CJK comma width.
5. Hindi neural ITN and punctuation (`altslate/*`) are days-old one-person releases, unchecked by native speakers: evidence for the approach, not a dependency.

## Rerunning

`myna-bench text --eval run1|run2` (`server/src/myna/benchmarker/text_eval.py`) scores arms per operation on these corpora and needs no network, snap or root for the built-in arms. Arm specs are in the module docstring.

```
  # English tidy: the tagger directly, against the identity baseline
myna-bench text --eval run2 --op tidy --lang en \
    --arm rules=identity --arm tagger:/path/to/disfluency-en:0.9
  # the same tagger through a running myna-text daemon
myna-bench text --eval run2 --op tidy --lang en \
    --arm rules=identity --arm socket:/path/to/myna-text.sock:myna-disfluency-en
  # any OpenAI-compatible endpoint (llama-server), few-shot or the bare LoRA task prompt
myna-bench text --eval run2 --arm rules=identity --arm 'openai:http://127.0.0.1:8080#qwen3-0.6b'
myna-bench text --eval run2 --arm rules=identity --arm 'lora=openai:http://127.0.0.1:8081#q17mt#task'
  # score recorded outputs (one <arm>.jsonl of {id, output, ms} per arm) next to live arms
myna-bench text --eval run2 --results chug-run2 --report report.md
```

- The arm named `rules` is the baseline of dAcc and of hybrids; every model arm (`openai:`) also gets a `hyb:` row.

- Checked end to end on 2026-11-25 (chug, run 2 tidy en): the model arm reproduces the experiment (Qwen3-0.6B few-shot over llama-server: acc 71.4, brk 22, dmg 30.7; the experiment: 71.4, 22, 31). The shipped tagger alone scores 47.6 because it was built to sit behind the English rule; the Rust rule alone scores 73.0 and 82.5 with the tagger (the removed rule is not rerunnable from here, see [the deleted steps](#the-deleted-steps)).
- Run 1 drops the few-shot examples from its items; run 2 does not, as in the experiment.
- Heavy runs (a model arm over all 2532 items takes minutes to an hour) belong on chug, one llama-server per arm with its own port and thread count, never on the laptop.
- A model arm over the Translate suites is the existing `myna-bench text --suite translate_en_<l>`, not `--eval`.
