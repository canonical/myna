# Preface

Read this before changing which model family Myna recommends, the language table behind it, the languages each family covers, or how the user's language is read.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

`myna_core::language` holds a table of error rates per (language, family): Parakeet's FLEURS WER for its 25 languages, SenseVoice-Small (the FunASR snap) against Whisper-small on Common Voice for zh, yue, ja, ko and en, and for hi Myna's own FLEURS runs of IndicConformer against Whisper. The lowest rate among the candidate families wins; Whisper covers every language the table does not measure. `myna_core::locale` turns the session into an ordered list of `ll` or `ll_CC` languages behind the `LocaleSource` port.

Next to the table, `ModelFamily::languages` lists what each family transcribes (Parakeet's 25, SenseVoice-Small's 5, IndicConformer's one per shipped component, Whisper's 99 in its tokenizer's order) and `endonym` names every covered language in itself. Settings shows languages only by endonym, never by flag: a language is not a nation.

Sources, checked 2026-09-28:

- Parakeet: the `nvidia/parakeet-tdt-0.6b-v3` model card, FLEURS WER, its full 25-language list.
- FunASR and Whisper: the SenseVoice paper (arXiv 2407.04051) Table 6, SenseVoice-Small against Whisper-small on Common Voice (CER for zh, yue, ja and ko, WER for en).
- IndicConformer and Whisper for hi (2026-11-17): `myna-bench run` on the whole FLEURS hi_in test split, myna-indicconformer on CPU (12.02%) against myna-whisper's large-v3-turbo on its GPU engine (31.47%), its most accurate model; small scored 57%. No published card number exists for either on this split (`server/.kb/model-evaluation.md`).

Caveats the table does not show:

- myna-funasr ships SenseVoice-Small, not Fun-ASR-Nano. The released Small checkpoint covers only zh, en, yue, ja and ko; the "50+ languages" on its card belong to the unreleased Large.
- zh_TW goes to FunASR although SenseVoice writes Simplified script. Whether Traditional-script users are better served by Whisper with a language hint is untested.
- The Whisper figures are for small, the GPU default; CPU users get base, which is worse, so the table flatters Whisper for them. For hi the figure is large-v3-turbo, flattering Whisper further.
- myna-indicconformer covers a language only once it ships that language's model component; `INDICCONFORMER_LANGUAGES` lists the shipped ones, and the table must measure each (a test holds them together).

# Important

- Only the most preferred language decides. Later `LANGUAGE` entries are fallbacks the user settled for: `cy:en` is a Welsh speaker, not a Parakeet user.
- Rates are ordinal and cross-dataset even within a language (en sets Parakeet's FLEURS against Common Voice figures). Use them only to order families for one language; never trust the margin or compare across languages.
- Read the language from `g_get_language_names()` (the list that also picks the UI translation), never from the `LC_*` format categories, which carry the Formats region. AccountsService `User.Language` is read only when that list has no language (`LANG=C`), and English is assumed after that.
- A family's coverage is what its card lists, most widely spoken first, since the collapsed summary names the first few. Parakeet's, FunASR's and IndicConformer's must equal their measured languages in the table (a test holds them together); every covered language needs an endonym, capitalised as a language chooser lists it. `user_language` maps locale codes the cards spell otherwise (glibc's `nb` and `fil`, the legacy `iw`, `in`, `ji`) onto the card's code, or a Norwegian user gets a nameless pill and no highlighted language.
- Recommend on transcription quality alone, over every known family, never gated on store availability (Charles, 2026-09-28): myna-funasr is recommended for zh, yue, ja and ko before it is published, and its App Center link starts working when it is. Myna Settings asks in three places (Charles, 2026-09-29): `recommend_among` over the installed families puts the pill on General's list (Whisper for zh while FunASR is absent; a backend of no known family never gets it), and `recommend` over every family, when not installed, carries the pill in the Install more models dialog and a hint on its button ("A better model for 中文 is available"). The two can both say "Best for" at once; that is the intended reading, best installed and best overall. The setup wizard installs `recommend` over every family too (`onboarding::model_offer`), and installs nothing until the language is read; when that is Whisper, it also sets the spoken language to the user's, unless the user chose one.
