"""Prompts of the model arms of ``myna-bench text --eval``.

``fewshot`` is the instruction plus worked examples the few-shot arms were run
with. Every set holds one unchanged example, so the model sees that leaving
text alone is an answer. ``task`` is the bare ``task: <op>, language: <lang>``
system prompt the fine-tuned (LoRA) arms were trained on; the training recipe
stays in the workstream, not here. The few-shot examples are never corpus
items: ``text_eval`` drops any run 1 row whose input equals one.
"""

from __future__ import annotations

TASK = {
    "tidy": (
        "Remove speech disfluencies from dictated text: filler sounds, stutters, accidental "
        "word repetitions and self-corrections (keep only the corrected version). Do not "
        "rephrase, translate or add words. If the text is already fluent, return it "
        "unchanged."
    ),
    "numbers": (
        "Rewrite spoken numbers in dictated text as a careful editor would write them: digits "
        "for amounts, times, dates, percentages, money, measurements and numbers of 10 or "
        "more; small numbers in ordinary prose stay as words. Change nothing else. Idioms "
        "such as 'one of' stay as words. If nothing needs changing, return the text "
        "unchanged."
    ),
    "addresses": (
        "Rewrite spoken web addresses and email addresses in dictated text in their written "
        "form (example.com, name@example.org, example.com/path). Change nothing else. If the "
        "text contains no spoken address, return it unchanged."
    ),
    "commands": (
        "The text was dictated. Carry out spoken editing commands that stand as their own "
        "sentence or lead a sentence before a comma: 'scratch that' deletes the sentence "
        "before it, 'new line' inserts a line break, 'new paragraph' inserts a blank line, "
        "'delete last word' and 'delete the last N words' remove words, 'format as a list' "
        "turns the following sentences into a list, 'quote ... end quote' puts the words in "
        "quotation marks. The same words used as part of an ordinary sentence are not "
        "commands. Remove the command words themselves. Return only the resulting text."
    ),
    "casing": (
        "The text was dictated by a programmer. A spoken casing command (camel case, pascal "
        "case, snake case, kebab case, constant case, title case, all caps, lower case, no "
        "spaces) applies to the words after it up to the next punctuation mark: join and case "
        "them accordingly and drop the command words. Words that only talk about casing are "
        "not commands. Change nothing else."
    ),
    "emoji": (
        "Replace a spoken emoji (an emoji name followed by the word 'emoji') with the emoji "
        "character itself. Text that merely talks about emoji is left as is. Change nothing "
        "else."
    ),
}

SYSTEM = (
    "You are a text post-processor inside a dictation program. {task} "
    "The text is in {language}; answer in the same language. "
    "Reply with the processed text only, no explanation, no quotes."
)

LANGUAGE = {"en": "English", "fr": "French", "de": "German", "es": "Spanish"}

SHOTS = {
    ("tidy", "en"): [
        ("So, um, we we need to book the the venue.", "So, we need to book the venue."),
        ("Call her on Friday, no, on Saturday.", "Call her on Saturday."),
        ("I didn't say that, I said the opposite.", "I didn't say that, I said the opposite."),
    ],
    ("tidy", "fr"): [
        ("Alors, euh, il faut faut réserver la salle.", "Alors, il faut réserver la salle."),
        ("Appelle-la vendredi, non, samedi.", "Appelle-la samedi."),
        ("Ils se sont vus hier soir.", "Ils se sont vus hier soir."),
    ],
    ("tidy", "de"): [
        ("Also, äh, wir wir müssen den Raum buchen.", "Also, wir müssen den Raum buchen."),
        ("Ruf sie am Freitag an, nein, am Samstag.", "Ruf sie am Samstag an."),
        (
            "Das ist die Frau, die die Blumen gekauft hat.",
            "Das ist die Frau, die die Blumen gekauft hat.",
        ),
    ],
    ("tidy", "es"): [
        (
            "Bueno, eh, tenemos tenemos que reservar la sala.",
            "Bueno, tenemos que reservar la sala.",
        ),
        ("Llámala el viernes, no, el sábado.", "Llámala el sábado."),
        ("No lo dije así, dije lo contrario.", "No lo dije así, dije lo contrario."),
    ],
    ("numbers", "en"): [
        (
            "The flight leaves at seven forty five and costs three hundred dollars.",
            "The flight leaves at 7:45 and costs $300.",
        ),
        ("Only two of the forty eight samples failed.", "Only two of the 48 samples failed."),
        ("She is one of the best players I know.", "She is one of the best players I know."),
    ],
    ("numbers", "fr"): [
        (
            "Le train part à sept heures quarante-cinq et coûte trente euros.",
            "Le train part à 7 h 45 et coûte 30 €.",
        ),
        (
            "Seuls deux des quarante-huit échantillons ont échoué.",
            "Seuls deux des 48 échantillons ont échoué.",
        ),
        (
            "C'est l'un des meilleurs joueurs que je connaisse.",
            "C'est l'un des meilleurs joueurs que je connaisse.",
        ),
    ],
    ("numbers", "de"): [
        (
            "Der Zug fährt um sieben Uhr fünfundvierzig und kostet dreißig Euro.",
            "Der Zug fährt um 7:45 Uhr und kostet 30 €.",
        ),
        (
            "Nur zwei der achtundvierzig Proben sind durchgefallen.",
            "Nur zwei der 48 Proben sind durchgefallen.",
        ),
        (
            "Er ist einer der besten Spieler, die ich kenne.",
            "Er ist einer der besten Spieler, die ich kenne.",
        ),
    ],
    ("numbers", "es"): [
        (
            "El tren sale a las siete y cuarenta y cinco y cuesta treinta euros.",
            "El tren sale a las 7:45 y cuesta 30 €.",
        ),
        (
            "Solo dos de las cuarenta y ocho muestras fallaron.",
            "Solo dos de las 48 muestras fallaron.",
        ),
        (
            "Es uno de los mejores jugadores que conozco.",
            "Es uno de los mejores jugadores que conozco.",
        ),
    ],
    ("addresses", "en"): [
        ("Write to maria dot lopez at acme dot org.", "Write to maria.lopez@acme.org."),
        (
            "The docs are at python dot org slash downloads.",
            "The docs are at python.org/downloads.",
        ),
        ("We met at the cafe near the station.", "We met at the cafe near the station."),
    ],
    ("addresses", "fr"): [
        ("Écris à maria point lopez arobase acme point org.", "Écris à maria.lopez@acme.org."),
        (
            "La doc est sur python point org slash downloads.",
            "La doc est sur python.org/downloads.",
        ),
        ("On s'est vus au café près de la gare.", "On s'est vus au café près de la gare."),
    ],
    ("addresses", "de"): [
        ("Schreib an maria Punkt lopez at acme Punkt org.", "Schreib an maria.lopez@acme.org."),
        (
            "Die Doku ist auf python Punkt org Schrägstrich downloads.",
            "Die Doku ist auf python.org/downloads.",
        ),
        (
            "Wir haben uns im Café am Bahnhof getroffen.",
            "Wir haben uns im Café am Bahnhof getroffen.",
        ),
    ],
    ("addresses", "es"): [
        ("Escribe a maria punto lopez arroba acme punto org.", "Escribe a maria.lopez@acme.org."),
        (
            "La documentación está en python punto org barra downloads.",
            "La documentación está en python.org/downloads.",
        ),
        (
            "Nos vimos en el café cerca de la estación.",
            "Nos vimos en el café cerca de la estación.",
        ),
    ],
    ("commands", "en"): [
        (
            "The meeting is on Monday. Scratch that. The meeting is on Tuesday.",
            "The meeting is on Tuesday.",
        ),
        (
            "Dear Anna, new paragraph. Thanks for the photos.",
            "Dear Anna,\n\nThanks for the photos.",
        ),
        ("He told me to delete that file.", "He told me to delete that file."),
    ],
    ("commands", "fr"): [
        ("La réunion est lundi. Efface ça. La réunion est mardi.", "La réunion est mardi."),
        (
            "Chère Anna, nouveau paragraphe. Merci pour les photos.",
            "Chère Anna,\n\nMerci pour les photos.",
        ),
        ("Il m'a dit d'effacer ce fichier.", "Il m'a dit d'effacer ce fichier."),
    ],
    ("commands", "de"): [
        (
            "Das Treffen ist am Montag. Lösch das. Das Treffen ist am Dienstag.",
            "Das Treffen ist am Dienstag.",
        ),
        ("Liebe Anna, neuer Absatz. Danke für die Fotos.", "Liebe Anna,\n\nDanke für die Fotos."),
        (
            "Er hat mir gesagt, ich soll die Datei löschen.",
            "Er hat mir gesagt, ich soll die Datei löschen.",
        ),
    ],
    ("commands", "es"): [
        ("La reunión es el lunes. Borra eso. La reunión es el martes.", "La reunión es el martes."),
        (
            "Querida Anna, nuevo párrafo. Gracias por las fotos.",
            "Querida Anna,\n\nGracias por las fotos.",
        ),
        ("Me dijo que borrara ese archivo.", "Me dijo que borrara ese archivo."),
    ],
    ("casing", "en"): [
        ("Rename it to camel case total item count.", "Rename it to totalItemCount."),
        ("The constant is screaming snake case max retries.", "The constant is MAX_RETRIES."),
        ("Lower case letters look friendlier.", "Lower case letters look friendlier."),
    ],
    ("emoji", "en"): [
        ("Great job party popper emoji", "Great job 🎉"),
        ("See you soon red heart emoji.", "See you soon ❤️."),
        ("Which emoji should I use here?", "Which emoji should I use here?"),
    ],
}


FEWSHOT = "fewshot"
TASK_FORMAT = "task"
FORMATS = (FEWSHOT, TASK_FORMAT)


def shot_inputs() -> frozenset[str]:
    return frozenset(heard for pairs in SHOTS.values() for heard, _ in pairs)


def supported(prompt: str, op: str, lang: str) -> bool:
    return prompt == TASK_FORMAT or (op, lang) in SHOTS


def messages(prompt: str, op: str, lang: str, text: str, think_off: bool) -> list[dict[str, str]]:
    suffix = " /no_think" if think_off else ""
    if prompt == TASK_FORMAT:
        return [
            {"role": "system", "content": f"task: {op}, language: {lang}"},
            {"role": "user", "content": text},
        ]
    system = SYSTEM.format(task=TASK[op], language=LANGUAGE[lang])
    out = [{"role": "system", "content": system}]
    for heard, written in SHOTS[(op, lang)]:
        out.append({"role": "user", "content": heard})
        out.append({"role": "assistant", "content": written})
    out.append({"role": "user", "content": text + suffix})
    return out
