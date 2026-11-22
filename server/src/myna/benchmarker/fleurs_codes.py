"""Mozilla Firefox Translations language codes -> FLEURS locale codes.

A translation plugin is evaluated on the FLEURS sentences of its target
language (``translate_en_<l>`` slices), so every language a catalog plugin
names must either map to a FLEURS locale or be explicitly ``no-eval`` (FLEURS
does not cover it); a language that is neither is a bug the tests catch.
"""

from __future__ import annotations

MOZILLA_TO_FLEURS: dict[str, str] = {
    "ar": "ar_eg",
    "az": "az_az",
    "be": "be_by",
    "bg": "bg_bg",
    "bn": "bn_in",
    "bs": "bs_ba",
    "ca": "ca_es",
    "cs": "cs_cz",
    "da": "da_dk",
    "de": "de_de",
    "el": "el_gr",
    "en": "en_us",
    "es": "es_419",
    "et": "et_ee",
    "fa": "fa_ir",
    "fi": "fi_fi",
    "fr": "fr_fr",
    "gu": "gu_in",
    "he": "he_il",
    "hi": "hi_in",
    "hr": "hr_hr",
    "hu": "hu_hu",
    "id": "id_id",
    "is": "is_is",
    "it": "it_it",
    "ja": "ja_jp",
    "kn": "kn_in",
    "ko": "ko_kr",
    "lt": "lt_lt",
    "lv": "lv_lv",
    "ml": "ml_in",
    "ms": "ms_my",
    "mt": "mt_mt",
    "nb": "nb_no",
    "nl": "nl_nl",
    "pl": "pl_pl",
    "pt": "pt_br",
    "ro": "ro_ro",
    "ru": "ru_ru",
    "sk": "sk_sk",
    "sl": "sl_si",
    "sr": "sr_rs",
    "sv": "sv_se",
    "ta": "ta_in",
    "te": "te_in",
    "th": "th_th",
    "tl": "fil_ph",
    "tr": "tr_tr",
    "uk": "uk_ua",
    "vi": "vi_vn",
    "zh-Hans": "cmn_hans_cn",
}

# Mozilla languages FLEURS has no locale for (nn: no Nynorsk; sq: no Albanian;
# zh-Hant: only Cantonese, a different language). Their plugins ship with a
# vendor score and the spot check, never a measured chrF.
NO_EVAL: frozenset[str] = frozenset({"nn", "sq", "zh-Hant"})


class NoEvalError(LookupError):
    """The language has no FLEURS locale, or is not known at all."""


def fleurs_code(lang: str) -> str:
    try:
        return MOZILLA_TO_FLEURS[lang]
    except KeyError:
        if lang in NO_EVAL:
            raise NoEvalError(f"{lang}: FLEURS has no locale; the plugin is no-eval") from None
        raise NoEvalError(f"{lang}: not in the Mozilla to FLEURS table") from None


def evaluable(lang: str) -> bool:
    return lang in MOZILLA_TO_FLEURS
