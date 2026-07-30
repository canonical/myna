# Myna System Test Plan (End-User Accuracy & Performance)

**Scope**: system-level, end-to-end testing only — a human presses the hotkey,
speaks, and judges what lands in a focused text field. This plan does **not**
cover unit tests, contract/wire tests, or the automated WER/CER harness
(`dev/bench.py`, `dev/matrix.py`, `pytest`) — those are already exercised
elsewhere and are out of scope here (see §11).

**Audience**: internal dev/QA team first (dry run), then opened to community
testers in a later wave. Crowd-testing submission tooling is intentionally
**not** specified in this document (deferred).

**Status of inputs used**: current model set (Whisper, Nemotron, Qwen3-ASR),
current streaming/batch mode implementation (features 007/008), and current
snap hardware verification status, as of 2026-07-29. See `docs/project-plan.md`
for the authoritative task tracker.

---

## 1. Purpose

Measure, from a real end user's perspective:

- **Accuracy**: does the transcribed/injected text match what was actually
  said, across languages, models, and vocabulary types a real user will use?
- **Performance**: does dictation *feel* responsive — from hotkey press to
  text appearing, and from end-of-speech to final committed text — across
  hardware a real user might own?

This plan produces **directional, human-judged** results. It is deliberately
not a replacement for the automated WER/CER benchmark suite, which already
provides precise, repeatable numbers per model/config
(`results/bench.jsonl`, `results/streaming-watermarks.json`). Where this plan's
subjective findings and the automated numbers disagree, trust the automated
numbers for the metric itself, and treat this plan's findings as a signal that
something needs to be re-benchmarked.

---

## 2. Roles & environment prerequisites

**Tester role**: no engineering background required beyond following setup
instructions; must be a fluent/native speaker of the language they're testing
in (accuracy judgments on non-native speech are not meaningful signal for
this round).

**Required environment**:

- Ubuntu 24.04 (or another `core24`-based snap environment).
- **GNOME on Wayland.** This is the only desktop environment currently
  verified for the hotkey + IBus injection + indicator stack. X11/XWayland
  and non-GNOME compositors (KDE, wlroots) are **not** supported test targets
  for this round — do not file accuracy/performance bugs against them; a
  single "doesn't work outside GNOME/Wayland" note is sufficient if tried.
- `myna` client snap installed, plus at least one of: `whisper-snap`,
  `nemotron-snap`, `qwen-snap` (install whichever models are under test — see
  §3).
- A working microphone, tested independently (e.g. via GNOME Settings sound
  input meter) before starting — mic problems should not be logged as myna
  bugs.
- Hotkey configured per `docs/desktop-injection.md` (default `Super+D`,
  toggle- or hold-to-talk depending on install path).

**Hardware tiers to cover** (tester self-reports actual specs; laptop or
desktop doesn't matter):

- **CPU-only** machine (no supported NVIDIA GPU present, or GPU snap not
  installed).
- **NVIDIA GPU** machine (CUDA-capable card, GPU-enabled snap variant
  installed where available — currently whisper and nemotron ship GPU
  engines; qwen is CPU-only regardless of hardware).

Record exact CPU model, RAM, and GPU model (if any) in the results table
(§9) — this is the closest thing this project currently has to a hardware-tier
report (T12 is still open on the engineering side).

---

## 3. Models under test

| Model | License | Language coverage | Mode support (shipped) | Snap | Notes for testers |
|---|---|---|---|---|---|
| **Whisper** (faster-whisper) | MIT | Multilingual (`*`) on non-`.en` checkpoints; English-only on `.en` checkpoints | Batch + streaming (local-agreement) | `whisper-snap` | CPU and NVIDIA GPU engines both shipped |
| **Nemotron** (FastConformer) | — | **English only** | Batch (native streaming loop still in development) | `nemotron-snap` | NVIDIA GPU only — no CPU engine exists |
| **Qwen3-ASR** (`qwen-c`) | Apache-2.0 | 30 languages: zh, en, yue, ar, de, fr, es, pt, id, it, ko, ru, th, vi, ja, tr, hi, ms, nl, sv, da, fi, pl, cs, fil, fa, el, ro, hu, mk | Batch (streaming exists but is sub-realtime on weaker CPUs — expect it to lag) | `qwen-snap` | CPU only in this shipped build |

**Expected failure modes** (not bugs — record as "expected" if observed):

- Nemotron given non-English speech: expect garbled or empty output.
- Any model given a language outside its supported set (e.g. a language
  outside Qwen's 30 list) via a language not English: expect garbled,
  empty, or misrecognized-as-a-different-language output.
- Qwen streaming mode on a modest CPU: expect visibly laggy/delayed partials,
  not necessarily wrong text.

---

## 4. Reading sample corpus (English, plus a 9-language diversity set)

The full English reading sample corpus (six passage categories, plus two
short non-English probe sentences for TC-07/TC-08) lives in a standalone
file: **[`docs/test-samples-en.md`](test-samples-en.md)**.

The same six-section corpus (§1–§6) has also been translated into 9
additional languages, each in its own file, to cover script/phonetic
diversity and give Qwen3-ASR and Whisper real accuracy signal beyond
English. **All translations are draft, machine-assisted, and require
native/fluent-speaker review before use** (per §2's fluent-speaker
requirement) — do not run a test session against an unreviewed file.
Product-specific terms (myna, whisper-snap, nemotron-snap, PipeWire, IBus,
Nemotron, GNOME Shell, version strings, file paths) are kept in English in
every translated file, matching how a real bilingual user would actually
speak them.

| Language | File | Notes |
|---|---|---|
| English | [`test-samples-en.md`](test-samples-en.md) | Reviewed/canonical; also holds the §7 unsupported-language probes |
| Spanish (es) | [`test-samples-es.md`](test-samples-es.md) | Needs native review |
| French (fr) | [`test-samples-fr.md`](test-samples-fr.md) | Needs native review; shares its §5 pangram with the TC-07 probe sentence |
| Italian (it) | [`test-samples-it.md`](test-samples-it.md) | Needs native review |
| Portuguese (pt) | [`test-samples-pt.md`](test-samples-pt.md) | Needs native review; confirm European vs. Brazilian variant preference |
| German (de) | [`test-samples-de.md`](test-samples-de.md) | Needs native review |
| Czech (cs) | [`test-samples-cs.md`](test-samples-cs.md) | Needs native review |
| Arabic (ar) | [`test-samples-ar.md`](test-samples-ar.md) | Needs native review; RTL — also a signal for bidirectional text/injection handling around English product terms; confirm MSA vs. dialect preference |
| Hindi (hi) | [`test-samples-hi.md`](test-samples-hi.md) | Needs native review; Devanagari — frequent script-mixing with English product terms is expected, not an error |
| Mandarin (zh) | [`test-samples-zh.md`](test-samples-zh.md) | Needs native review; Simplified Chinese draft — confirm Traditional preference per tester |

Each language file mirrors `test-samples-en.md`'s structure — quick index
(section numbers are consistent across all files):

- §1 — Natural long-form prose (2 passages: A, B) — used by TC-01, TC-10, TC-11
- §2 — Command / short-utterance set (10 lines) — used by TC-03
- §3 — Domain / technical vocabulary passage — used by TC-04
- §4 — Numbers, dates, and punctuation-heavy passage — used by TC-05
- §5 — Pangram / phonetic smoke-test — used by TC-06
- §6 — Long continuous passage for streaming tests (30s+) — used by TC-02, §9
- §7 (English file only) — Unsupported-language probes: French sentence
  (TC-07, Nemotron) and Estonian sentence (TC-08, Qwen only) — the Estonian
  sentence still needs native/fluent-speaker review before use

**Model applicability per language**: all 9 non-English languages above are
within Qwen3-ASR's 30-language list, so each can be tested on both
Qwen3-ASR and Whisper (Nemotron remains English-only per §3 and is
excluded from all non-English runs except the TC-07 failure probe).

---

## 5. Test matrix

Cross the following dimensions. Not every cell applies to every model — use
§3's mode/language support to skip inapplicable combinations (e.g. Nemotron
has no CPU engine; Qwen has no GPU engine in this build).

| Dimension | Values |
|---|---|
| Model | Whisper, Nemotron, Qwen3-ASR |
| Language | English (all passages, `test-samples-en.md` §1–§6); Spanish, French, Italian, Portuguese, German, Czech, Arabic, Hindi, Mandarin (full §1–§6 corpus per language, pending native review — see §4 table); French and Estonian (two short probe sentences only, `test-samples-en.md` §7, TC-07/TC-08) |
| Mode | `batch`, `streaming`, `auto` (real-world default) |
| Hardware | CPU-only, NVIDIA GPU |
| Injection target app | one plain text field (e.g. GNOME Text Editor / gedit), one "real" app (e.g. browser address bar, LibreOffice Writer) |

**Minimum required run per tester**: for each installed model, read the
English passages in `test-samples-en.md` §1–§6 at least once in `auto`
mode on their available hardware, into at least one plain-text app.
Testers who are fluent/native speakers of one of the 9 additional languages
in §4 (and whose language file has passed native-speaker review) should
also cover that language's §1–§6 corpus at least once. Testers with time to
cover more of the matrix (multiple modes, multiple apps, multiple
languages, both hardware tiers if they have access to both) should do so
and note it in the results table.

**Same-speaker consistency**: where possible, have the *same* tester read the
*same* passage across different models/modes/hardware so that differences
are attributable to the model/config, not the speaker. If multiple testers
are available, have them all read the same passages — divergence in results
across testers on the same passage/model is itself signal about
accent/speaker-diversity sensitivity (a known gap — the automated real-speech
corpus currently has low speaker diversity).

**Pass/fail reference tolerances** (reused from existing engineering
watermarks, since none exist specifically for human judgment yet):

- Streaming vs. batch accuracy should feel roughly comparable — engineering
  watermark tolerance is ≤2 percentage points WER delta. As a human, this
  translates to: streaming should not introduce noticeably more wrong words
  than batch on the same passage.
- Once text is shown as committed (not `~`-prefixed unstable text), it must
  **never change or disappear**. Any committed text flickering, being
  overwritten, or vanishing is a **fail**, not a rough edge.
- No duplicated or repeated phrases at commit boundaries in streaming mode.

---

## 6. Detailed test cases

Each test case below follows: **Description**, **Preconditions**, **Steps**,
**Expected Result**. These instantiate the dimensions in §5's matrix into
concrete, repeatable procedures. Testers should log the outcome of each case
they run in the §9 results table, using the Test Case ID for traceability.

**Language repetition**: TC-01 through TC-06, TC-10, and TC-11 are written
against `test-samples-en.md` for readability, but should be repeated once
per language a tester is fluent in and has an approved (reviewed)
translation file for — see the §4 table for the full list of language
files. Substitute the corresponding section of the language file being
tested wherever a step references `test-samples-en.md`. Do not run these
test cases against a translation file that has not yet had native-speaker
review (see §4 review-status notes per file).

### TC-01 — Batch mode, natural long-form dictation

**Description**: Baseline accuracy check — dictate natural connected prose
in the product's default-adjacent mode (`batch`) and compare against the
reference transcript.

**Preconditions**: Model installed (repeat per model); hotkey configured;
plain-text app (e.g. GNOME Text Editor) focused and empty; mode explicitly
set to `batch`; language file for the language under test has passed
native-speaker review (see §4).

**Steps**:
1. Open the target app, place the cursor in an empty document.
2. Press the hotkey to start recording.
3. Read Passage A (§1 of the language file under test, e.g.
   `test-samples-en.md` for English) aloud at a natural pace, in one take.
4. Stop recording (release/press hotkey per install mode).
5. Wait for the transcript to finish appearing in the document.
6. Compare the injected text word-for-word against the reference transcript
   in that same §1.

**Expected Result**: Injected text matches the reference transcript with no
more than a small number of minor word errors (a handful of substitutions is
acceptable; missing sentences, garbled runs of text, or empty output are
not). Text appears only once, in full, after speech ends — no partial/
flickering text during recording in batch mode.

---

### TC-02 — Streaming mode, long continuous passage, commit behavior

**Description**: Exercises streaming mode's progressive emission across
multiple commit boundaries and natural pauses, and verifies the
unstable/committed distinction is respected end-to-end.

**Preconditions**: Model with streaming support installed (Whisper or
Qwen3-ASR — see §3); mode set to `streaming`; `--show-unstable` (or desktop
equivalent) enabled; plain-text app focused and empty; language file for
the language under test has passed native-speaker review (see §4).

**Steps**:
1. Press the hotkey to start recording.
2. Read the long continuous passage (§6 of the language file under test)
   aloud at a natural pace, including its natural sentence-boundary pauses
   — do not read it as disconnected fragments.
3. Observe the app during recording: note when unstable text first appears,
   and note each point where text transitions from unstable to committed.
4. Stop recording once the full passage has been read.
5. Compare the final injected text against the reference transcript in that
   same §6.

**Expected Result**: Unstable text is visually distinguishable (e.g. `~`
prefix or preedit styling) and disappears/resolves into committed text
without ever being the thing actually written into the document. Once a
span of text is committed, it does not change, vanish, or get overwritten
later in the session. No phrase is duplicated at a commit boundary. The
final transcript's accuracy is comparable to the same passage read in batch
mode (TC-01-style comparison) — not meaningfully worse.

---

### TC-03 — Command / short-utterance accuracy

**Description**: Verifies accuracy on short, discrete, realistic dictation
commands rather than long-form prose — the shape of speech this product is
actually built around (hotkey → short phrase → inject).

**Preconditions**: Model installed; mode `auto`; plain-text app focused and
empty; language file for the language under test has passed native-speaker
review (see §4).

**Steps**:
1. For each of the 10 lines in §2 of the language file under test, in order:
   a. Press the hotkey.
   b. Speak the single line.
   c. Stop recording.
   d. Note the injected result on a new line in the document.
2. After all 10 lines, compare each injected line against its reference
   text in that same §2.

**Expected Result**: Each utterance is transcribed independently and
correctly, including short imperative phrases and the punctuation-command
line ("Comma, new paragraph, period.") — note whether spoken punctuation is
transcribed literally (expected, since spoken-punctuation-as-command is not
a current documented feature) or interpreted as an actual comma/paragraph
break/period (would indicate an undocumented capability, log as a note, not
a bug). No utterance is dropped, merged with the previous one, or duplicated.

---

### TC-04 — Domain/technical vocabulary accuracy

**Description**: Verifies the model's handling of proper nouns, acronyms,
version strings, and file paths specific to this product's own domain — a
category ASR models commonly mangle.

**Preconditions**: Model installed; mode `auto`; plain-text app focused and
empty; language file for the language under test has passed native-speaker
review (see §4).

**Steps**:
1. Press the hotkey and read the domain/technical passage (§3 of the
   language file under test) aloud in one take, pronouncing product terms
   naturally (not spelled out letter-by-letter).
2. Stop recording and wait for the transcript.
3. Compare specifically the technical terms (PipeWire, myna-desktop, IBus,
   Nemotron, "one point three point zero", the file path — kept in English
   in every language file per §4's convention) against the reference text —
   treat these terms as the primary signal, not overall prose fluency.

**Expected Result**: Common English words transcribe correctly (high bar).
Product-specific terms and the version string/file path are recorded as
"correct", "phonetically close" (e.g. "pipe wire" instead of "PipeWire" —
log as a minor/expected miss, not a failure), or "unrecognizable" (log as a
failure) — this test case's primary output is a per-term scorecard rather
than a single pass/fail.

---

### TC-05 — Numbers, dates, and punctuation normalization

**Description**: Verifies real spoken numbers/dates/punctuation transcribe
in a form a human would consider correct, independent of how the automated
scoring's normalization rules treat them.

**Preconditions**: Model installed; mode `auto`; plain-text app focused and
empty; language file for the language under test has passed native-speaker
review (see §4).

**Steps**:
1. Press the hotkey and read the numbers/dates passage (§4 of the language
   file under test) aloud in one take, at a natural pace (do not
   artificially slow down for the digits).
2. Stop recording and wait for the transcript.
3. Compare the phone number, date, currency amount, time, and confirmation
   code against the reference text in that same §4.

**Expected Result**: Each numeric/date/currency span is either transcribed
in digit form ("555-0142", "July 29th, 2026") or in an equivalent spelled-out
form that a human reader would judge as correct — log the exact form
produced per span, since this is directly useful signal for whether the
existing NFKC/casefold/punctuation normalization used in automated scoring
matches what users actually see. Digit transpositions, wrong dates, or
dropped spans are failures.

---

### TC-06 — Pangram smoke test

**Description**: Fast canary check to run before a full session and after
any configuration change (model, mode, hardware) — should take under 10
seconds to execute and judge.

**Preconditions**: Model installed; any mode; plain-text app focused and
empty; language file for the language under test has passed native-speaker
review (see §4).

**Steps**:
1. Press the hotkey.
2. Say the pangram/phonetic sentence from §5 of the language file under
   test (e.g. "The quick brown fox jumps over the lazy dog." for English).
3. Stop recording.
4. Compare the injected text against the reference sentence.

**Expected Result**: Exact or near-exact match. This sentence previously
caught a Nemotron empty-output bug on synthetic audio — any empty, garbled,
or wildly incorrect output here is a strong signal to stop and investigate
before continuing with longer passages, rather than a minor issue to note
and move past.

---

### TC-07 — Unsupported-language probe: Nemotron given non-English speech

**Description**: Confirms Nemotron's documented English-only limitation
fails gracefully (garbled or empty output) rather than silently producing
plausible-looking wrong text, using the French sentence in
`test-samples-en.md` §7.1.

**Preconditions**: Nemotron installed; plain-text app focused and empty.

**Steps**:
1. Press the hotkey to start recording.
2. Read the French sentence in `test-samples-en.md` §7.1 aloud, at a
   natural pace.
3. Stop recording and observe the injected result.

**Expected Result**: Garbled or empty output, not a plausible-but-wrong
English transcript — the failure should be obvious to the tester, not
silently misleading. Log the exact observed output verbatim.

---

### TC-08 — Unsupported-language probe: language outside Qwen's 30-language list

**Description**: Confirms behavior when Qwen3-ASR is given a language it
was never trained to support, using the Estonian sentence in
`test-samples-en.md` §7.2. **Scoped to Qwen3-ASR only** — Whisper's
multilingual checkpoints support Estonian, so running this probe against
Whisper would not demonstrate an out-of-vocabulary failure.

**Preconditions**: Qwen3-ASR (`qwen-snap`) installed; plain-text app
focused and empty.

**Steps**:
1. Press the hotkey to start recording.
2. Read the Estonian sentence in `test-samples-en.md` §7.2 aloud, at a
   natural pace.
3. Stop recording and observe the injected result.

**Expected Result**: Garbled, empty, or misidentified-as-a-different-language
output; log the exact observed behavior verbatim since this shapes future
error-taxonomy work (T31).

---

### TC-09 — Secure-field injection behavior (GNOME/Wayland)

**Description**: Tracks the known, currently unresolved gap where myna
cannot distinguish a password field from a normal text field on
GNOME/Wayland. See §8 for full background — this test case is the
executable form of that check.

**Preconditions**: Any model installed; a GNOME/Wayland-native app with a
password field available (e.g. GNOME Settings → change password dialog).

**Steps**:
1. Focus the password field.
2. Press the hotkey and say: "test password one two three" (never a real
   password).
3. Stop recording.
4. Observe whether text appears in the password field.

**Expected Result**: Currently expected (not a new bug): the dictated text
is injected into the password field, since secure-field detection does not
reach the IBus injector on GNOME/Wayland today. Log the exact observed
behavior every run — this is a tracked awareness test, and any change in
behavior (e.g. injection now correctly refused) is itself a significant
signal worth flagging prominently.

---

### TC-10 — Cross-app injection consistency

**Description**: Verifies dictation and injection work consistently across
more than one real application, not just a single reference text editor.

**Preconditions**: Model installed; mode `auto`.

**Steps**:
1. Repeat TC-01 (Passage A) with the target app set to a browser address
   bar or search box.
2. Repeat TC-01 (Passage A) again with the target app set to a
   full document editor (e.g. LibreOffice Writer).
3. Compare both results against the GNOME Text Editor result from the
   original TC-01 run.

**Expected Result**: Transcription accuracy is consistent across apps (the
model doesn't know or care which app has focus) — any app-specific
difference indicates an injection bug (e.g. dropped characters, wrong
cursor position, focus stolen mid-dictation) rather than a model accuracy
issue, and should be logged as such. Also worth noting for non-Latin-script
or RTL languages (Arabic, Hindi, Mandarin): confirm cursor position and
text direction render correctly across apps, since app-specific text
direction/IME handling bugs are a distinct risk from model accuracy.

---

### TC-11 — Hardware tier comparison (CPU vs. GPU)

**Description**: Directional comparison of accuracy and perceived latency
for the same model/passage across a CPU-only machine and an NVIDIA GPU
machine, where the tester has access to both.

**Preconditions**: Same model installed on both a CPU-only and an NVIDIA
GPU machine; same tester reads the same passage on both.

**Steps**:
1. On the CPU-only machine, run TC-01 (Passage A) in `auto` mode; record the
   §7 performance observations alongside the transcript.
2. Repeat identically on the NVIDIA GPU machine.
3. Compare both transcripts against each other and against the reference
   text, and compare the two sets of performance observations.

**Expected Result**: Accuracy should be effectively identical between
hardware tiers for the same model (hardware should not change *what* is
transcribed, only *how fast*). Any accuracy divergence between tiers is a
fail worth escalating. Latency/responsiveness differences are expected and
should simply be recorded (e.g. GPU noticeably faster time-to-first-text),
not treated as a failure.

---

## 7. Performance observations

**Framing**: these are subjective, stopwatch-or-feel observations from a
real user's perspective, not a formal benchmark. No ratified latency SLOs
exist yet for this project (hardware-tier performance contract is still an
open engineering item) — this section's job is to surface *directional*
problems (e.g. "GPU feels instant, CPU has a multi-second pause"), not to
produce citable numbers.

For each test run, note (rough categories are fine — "instant" /
"noticeable but acceptable" / "slow enough to be annoying"):

1. **Hotkey press → indicator appears**: how quickly does the user get
   feedback that recording has started?
2. **End of speech → first text appears**:
   - Batch mode: time until the full committed transcript appears.
   - Streaming mode: time until the first unstable (`~`) text appears, and
     separately, time until the first committed text appears.
3. **Overall session feel**: did the system ever seem to hang, drop audio,
   or require the user to repeat themselves?
4. **System load side-effects**: noticeable fan noise, lag in other
   applications, or visible CPU/GPU load spikes during transcription (a
   rough proxy for load, since formal RTF measurement is not expected of
   testers).

---

## 8. Security / edge-case test: secure-field behavior

**Background**: on GNOME/Wayland, this project has a documented, currently
unresolved gap — the desktop's secure-field marking (used for password
fields) does not reach the IBus injector, meaning myna cannot currently tell
a password field apart from a normal text field. On X11/XWayland this
detection works correctly, but X11 is not a primary supported target in this
round.

**Test case** (log the observed outcome every time — this is a known issue,
not a surprise, but must be tracked so a future fix can be verified against
this same test):

1. Focus a password field in a GNOME/Wayland-native app (e.g. a login form,
   GNOME Settings password change dialog).
2. Trigger the hotkey and dictate a short phrase (do **not** use a real
   password — use e.g. "test password one two three").
3. Record whether myna:
   - Refuses to inject (correct/expected long-term behavior), or
   - Injects the dictated text into the password field (currently the
     expected/known outcome on GNOME/Wayland today).

This is not a blocking bug for this test round — it's an awareness/tracking
test. Do not spend extended time trying to work around it.

---

## 9. Streaming-specific checks

Using the long continuous passage (§6 of the language file under test, e.g.
`test-samples-en.md` for English — see §4 for the full list of language
files) in `streaming` mode with `--show-unstable` (or the desktop
equivalent, if exposed):

1. Confirm unstable text is visually distinguished (e.g. prefixed `~`, or
   shown as preedit) and is **never** the text actually injected into the
   focused app — only committed text should land in the document.
2. Confirm committed text is append-only: once text is committed, it does
   not change, get overwritten, or disappear later in the same session.
3. Confirm natural pauses/silence gaps in the passage do not cause the
   system to drop text, freeze, or duplicate the phrase spoken just before
   the pause.
4. Compare the final transcript against the same passage read in `batch`
   mode — flag if streaming's final result looks meaningfully worse (more
   than a couple of wrong words difference) than batch's.

---

## 10. Results reporting

Fill in one row per test run:

| Date | Tester | Test Case ID | Model | Language | Mode | Hardware (CPU/GPU, brief specs) | App | Passage(s) | Pass/Fail | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| | | | | | | | | | | |

Additional free-text notes to capture per run where relevant:
- Accent/native-language of tester (for attributing accuracy differences).
- Background noise conditions (quiet room vs. not).
- Any expected-failure cases from §3 that were observed (mark as
  "expected", not a bug).
- Secure-field observation from §8.

---

## 11. Out of scope

This test plan explicitly does **not** cover:

- Unit tests, contract/wire-protocol tests, or any part of the existing
  `pytest`/Rust test suites.
- Automated WER/CER/RTF benchmarking (`dev/bench.py`, `dev/matrix.py`,
  `dev/aggregate.py`) — those already exist and produce precise, repeatable
  numbers; this plan is a human-perspective complement, not a replacement.
- Non-GNOME desktop environments (KDE, wlroots compositors) and X11/XWayland.
- arm64 hardware (whisper-snap declares an arm64 build target, but it is
  unverified — out of scope until that changes).
- Crowd-testing submission tooling/process (deferred to a later document).
- Languages beyond the 9-language diversity set in §4 (Spanish, French,
  Italian, Portuguese, German, Czech, Arabic, Hindi, Mandarin) plus the two
  §7 probe sentences (TC-07 French, TC-08 Estonian) — further language
  expansion (e.g. covering the rest of Qwen's 30-language list) is
  deferred to a later round.
- Running any translated-corpus test session before its file has passed
  native/fluent-speaker review (see the per-file review-status notes in
  §4) — all 9 non-English corpus files are currently draft,
  machine-assisted translations.
- Formal, numeric latency SLOs (no ratified hardware-tier performance
  contract exists yet in this project).
