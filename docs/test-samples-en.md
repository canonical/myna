# Myna Test Reading Sample Corpus (English)

Six passage categories used by `docs/test-plan-system.md`. Non-English
translations are **explicitly deferred** pending review of this English
version — do not translate ad hoc.

Sourcing rule: long-form passages (#1, #6) come from **contemporary,
permissively-licensed real text** — Wikipedia/Wikinews (CC BY-SA / CC BY) or
US government publications (public domain as government works) — never
decades-old public-domain literature, which reads unnaturally when spoken
aloud by a modern speaker. Product-specific categories (#2–#4) are original,
hand-written, since no external source can supply myna-specific vocabulary.

Each passage below lists: the reference text (ground truth), an approximate
spoken duration, and a source/license note.

---

## 1. Natural long-form prose (2 passages)

Used to test connected, naturally-prosodic speech — not clipped,
over-enunciated word lists.

**Passage A** (~35 seconds spoken)

> Source: adapted from Wikipedia, "Artificial intelligence" article
> (CC BY-SA 4.0), current revision as of 2026.

> "Artificial intelligence research has focused on a few key goals: reasoning,
> knowledge representation, planning, learning, natural language processing,
> and perception. General intelligence, the ability to complete any task
> performable by a human, is among the field's long-term objectives. To reach
> these goals, researchers have used a wide range of techniques, including
> search and mathematical optimization, formal logic, artificial neural
> networks, and methods based on statistics, probability, and economics."

**Passage B** (~30 seconds spoken)

> Source: adapted from a public plain-language summary published by NIST
> (US government work, public domain), current guidance on cybersecurity
> basics.

> "Every organization that uses computers and networks faces a basic set of
> cybersecurity risks. Employees can help manage those risks by using strong,
> unique passwords, keeping software up to date, and being cautious with
> email attachments and links from unknown senders. Multi-factor
> authentication adds an extra layer of protection, even if a password is
> stolen. Regularly backing up important files means that a ransomware
> attack or hardware failure doesn't have to mean permanent data loss."

## 2. Command / short-utterance set

Realistic, discrete lines a dictation user would actually speak in a single
hotkey press. Read each **as a separate take** (press hotkey, speak one line,
release/stop).

1. "Open a new terminal window."
2. "Send this to the team by Friday."
3. "Comma, new paragraph, period."
4. "Undo that."
5. "Schedule a meeting for tomorrow at three PM."
6. "Reply: sounds good, see you then."
7. "Search for nearby coffee shops."
8. "Mute the microphone."
9. "New line. Thanks, talk soon."
10. "Cancel that, never mind."

## 3. Domain / technical vocabulary passage

Loaded with terms this project's actual users will say — proper nouns,
acronyms, package names, version strings, file paths.

> "I installed the myna dash desktop snap alongside whisper dash snap and
> nemotron dash snap, then confirmed PipeWire was routing my microphone
> correctly. The hotkey triggers IBus injection, and I enabled the preedit
> flag to preview unstable text before it commits. After upgrading to version
> one point three point zero, I checked the config at tilde slash dot config
> slash myna slash settings dot json to make sure streaming mode was still
> set to auto. The GNOME Shell extension shows the activity indicator without
> stealing focus from my terminal."

*(Read naturally — spell out "PipeWire", "myna-desktop", "IBus", "Nemotron"
as words, not letter-by-letter, unless that's how you'd normally say them.)*

## 4. Numbers, dates, and punctuation-heavy passage

Probes digit/date normalization and whether real spoken usage matches the
documented NFKC/casefold/punctuation normalization rules used in scoring.

> "Call me at five five five, oh one four two, on July twenty-ninth, two
> thousand twenty-six. The invoice total came to four hundred and twelve
> dollars and fifty cents, due within thirty days. My flight leaves at six
> forty-five AM from gate B twelve, and the confirmation code is X-Ray Tango
> four seven one."

## 5. Pangram / phonetic smoke-test

Short, phonetically dense — use as a quick canary before starting a full
session, and as a fast regression check between configuration changes.

> "The quick brown fox jumps over the lazy dog."

*(Optional second pangram if a quick second data point is useful: "Pack my
box with five dozen liquor jugs.")*

## 6. Long continuous passage for streaming tests (30s+)

A single uninterrupted, multi-sentence read — not disconnected clips — to
exercise multiple commit boundaries, natural silence gaps, and mid-sentence
pauses. Use this specifically for the streaming-mode checks (see
`docs/test-plan-system.md` §9).

> Source: adapted from Wikinews-style contemporary reporting text
> (CC BY 2.5) plus a US government (NASA, public domain) mission-update
> style paragraph, combined into one continuous read (~45–60 seconds).

> "Researchers announced this week that a new weather satellite has begun
> transmitting data from orbit, providing forecasters with higher-resolution
> imagery than previous generations of instruments. The satellite, launched
> earlier this year, carries sensors capable of tracking storm systems in
> near real time, which officials say should improve early warnings for
> coastal communities. Meanwhile, engineers at the mission's ground control
> center confirmed that all onboard systems are operating within expected
> parameters, and the spacecraft has successfully completed its first orbit
> adjustment maneuver. The next major milestone, a full calibration of the
> imaging instruments, is expected to be completed within the coming month,
> after which the satellite will begin routine operational service."

## 7. Unsupported-language probes

Two short failure-mode probe sentences — not full accuracy passages, and not
translations of the §5 pangram (a literal translation of an English pangram
usually isn't itself a pangram in the target language). Each targets a
specific documented model limitation from `docs/test-plan-system.md` §3.

**7.1 — French sentence, for TC-07 (Nemotron given non-English speech)**

Nemotron is English-only with no other-language support at all, so any
natural, clearly-spoken non-English sentence is sufficient to exercise this
probe — the specific language doesn't matter for TC-07, French was chosen
for tester availability.

> "Portez ce vieux whisky au juge blond qui fume."

*(A standard, well-known French pangram — used here simply as a natural,
phonetically clean French sentence, not because pangram density itself is
relevant to this probe.)*

**7.2 — Estonian sentence, for TC-08 (language outside Qwen's 30-language
list), Qwen only**

Estonian is confirmed **not** present in Qwen3-ASR's supported list (zh, en,
yue, ar, de, fr, es, pt, id, it, ko, ru, th, vi, ja, tr, hi, ms, nl, sv, da,
fi, pl, cs, fil, fa, el, ro, hu, mk — see `docs/test-plan-system.md` §3).
Whisper's multilingual checkpoints do support Estonian, so **this probe is
scoped to Qwen3-ASR only** — running it against Whisper would not
demonstrate an out-of-vocabulary failure.

> "Eile õhtul sadas Tallinnas tugevat vihma ja tänavad muutusid libedaks."

*(Draft content — **needs native/fluent Estonian speaker review before use**,
per §2's requirement that accuracy judgments be made only by a fluent/native
speaker of the language being tested. Rough gloss: "Last night it rained
heavily in Tallinn and the streets became slippery.")*
