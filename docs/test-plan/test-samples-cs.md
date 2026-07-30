# Myna Test Reading Sample Corpus (Czech / Čeština)

Czech (`cs`) translation of the reading sample corpus used by
`docs/test-plan-system.md`. Mirrors the structure of `docs/test-samples-en.md`
§1–§6.

> **Review status**: draft machine-assisted translation — **needs
> native/fluent Czech speaker review before use**, per
> `docs/test-plan-system.md` §2's requirement that accuracy judgments be
> made only by a fluent/native speaker of the language being tested.

**Product-specific terms convention**: product names, package names, and
technical identifiers (myna, whisper-snap, nemotron-snap, PipeWire, IBus,
Nemotron, GNOME Shell, version strings, file paths) are kept in **English**
throughout, matching how a real bilingual user would actually speak them —
these are not translated.

---

## 1. Natural long-form prose (2 passages)

**Passage A** (~35 seconds spoken)

> "Výzkum umělé inteligence se zaměřil na několik klíčových cílů:
> usuzování, reprezentaci znalostí, plánování, učení, zpracování
> přirozeného jazyka a vnímání. Obecná inteligence, tedy schopnost splnit
> jakýkoli úkol, který zvládne člověk, patří mezi dlouhodobé cíle tohoto
> oboru. Aby těchto cílů dosáhli, výzkumníci použili širokou škálu
> technik, včetně prohledávání a matematické optimalizace, formální
> logiky, umělých neuronových sítí a metod založených na statistice,
> pravděpodobnosti a ekonomii."

**Passage B** (~30 seconds spoken)

> "Každá organizace, která používá počítače a sítě, čelí základní
> množině rizik v oblasti kybernetické bezpečnosti. Zaměstnanci mohou
> pomoci tato rizika zvládat používáním silných a jedinečných hesel,
> udržováním softwaru aktuálního a opatrností vůči přílohám a odkazům od
> neznámých odesílatelů. Vícefaktorové ověřování přidává další vrstvu
> ochrany, i když je heslo odcizeno. Pravidelné zálohování důležitých
> souborů znamená, že útok ransomwaru nebo selhání hardwaru nemusí vést k
> trvalé ztrátě dat."

## 2. Command / short-utterance set

1. "Otevři nové okno terminálu."
2. "Pošli to týmu do pátku."
3. "Čárka, nový odstavec, tečka."
4. "Vrať to zpět."
5. "Naplánuj schůzku na zítra na třetí hodinu odpoledne."
6. "Odpověz: skvělé, tak zase někdy."
7. "Vyhledej kavárny v okolí."
8. "Vypni mikrofon."
9. "Nový řádek. Díky, brzy se ozvu."
10. "Zruš to, nech to být."

## 3. Domain / technical vocabulary passage

> "Nainstaloval jsem snap myna-desktop spolu s whisper-snap a
> nemotron-snap, a pak jsem potvrdil, že PipeWire správně směruje můj
> mikrofon. Klávesová zkratka spouští injekci IBus, a povolil jsem
> možnost preedit pro náhled nestabilního textu před jeho potvrzením. Po
> aktualizaci na verzi one point three point zero jsem zkontroloval
> konfiguraci na tilde slash dot config slash myna slash settings dot
> json, abych se ujistil, že streamovací režim je stále nastaven na
> auto. Rozšíření GNOME Shell zobrazuje indikátor aktivity, aniž by
> odebralo fokus mému terminálu."

## 4. Numbers, dates, and punctuation-heavy passage

> "Zavolej mi na pět pět pět, nula jedna čtyři dva, dvacátého devátého
> července dva tisíce dvacet šest. Celková částka faktury byla čtyři sta
> dvanáct dolarů a padesát centů, splatná do třiceti dnů. Můj let
> odlétá v šest čtyřicet pět ráno z brány B dvanáct, a potvrzovací kód
> je X-Ray Tango čtyři sedm jedna."

## 5. Pangram / phonetic smoke-test

> "Příliš žluťoučký kůň úpěl ďábelské ódy."

*(A standard, well-known Czech pangram, used for phonetic density rather
than as a literal translation of the English fox pangram.)*

## 6. Long continuous passage for streaming tests (30s+)

> "Vědci tento týden oznámili, že nová meteorologická družice začala
> vysílat data z oběžné dráhy a poskytuje meteorologům snímky s vyšším
> rozlišením než předchozí generace přístrojů. Družice, vypuštěná
> začátkem letošního roku, nese senzory schopné sledovat bouřkové
> systémy téměř v reálném čase, což by podle úředníků mělo zlepšit
> včasné varování pro pobřežní komunity. Mezitím inženýři v řídicím
> středisku mise potvrdili, že všechny palubní systémy fungují v rámci
> očekávaných parametrů a že kosmická loď úspěšně dokončila svůj první
> manévr úpravy dráhy. Další velký milník, úplná kalibrace zobrazovacích
> přístrojů, by měl být dokončen během příštího měsíce, poté družice
> zahájí rutinní provoz."
