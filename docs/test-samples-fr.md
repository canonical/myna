# Myna Test Reading Sample Corpus (French / Français)

French (`fr`) translation of the reading sample corpus used by
`docs/test-plan-system.md`. Mirrors the structure of `docs/test-samples-en.md`
§1–§6.

> **Review status**: draft machine-assisted translation — **needs
> native/fluent French speaker review before use**, per
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

> "La recherche en intelligence artificielle s'est concentrée sur quelques
> objectifs clés : le raisonnement, la représentation des connaissances, la
> planification, l'apprentissage, le traitement du langage naturel et la
> perception. L'intelligence générale, la capacité d'accomplir toute tâche
> réalisable par un humain, figure parmi les objectifs à long terme du
> domaine. Pour atteindre ces objectifs, les chercheurs ont utilisé un
> large éventail de techniques, notamment la recherche et l'optimisation
> mathématique, la logique formelle, les réseaux de neurones artificiels,
> ainsi que des méthodes fondées sur les statistiques, les probabilités et
> l'économie."

**Passage B** (~30 seconds spoken)

> "Toute organisation qui utilise des ordinateurs et des réseaux fait face
> à un ensemble de risques de cybersécurité de base. Les employés peuvent
> aider à gérer ces risques en utilisant des mots de passe forts et
> uniques, en maintenant les logiciels à jour, et en faisant attention aux
> pièces jointes et aux liens provenant d'expéditeurs inconnus.
> L'authentification multifacteur ajoute une couche de protection
> supplémentaire, même si un mot de passe est volé. Sauvegarder
> régulièrement les fichiers importants signifie qu'une attaque de
> rançongiciel ou une panne matérielle ne doit pas entraîner de perte
> définitive de données."

## 2. Command / short-utterance set

1. "Ouvre une nouvelle fenêtre de terminal."
2. "Envoie ceci à l'équipe avant vendredi."
3. "Virgule, nouveau paragraphe, point."
4. "Annule ça."
5. "Planifie une réunion pour demain à quinze heures."
6. "Réponds : ça marche, à bientôt."
7. "Cherche des cafés à proximité."
8. "Coupe le microphone."
9. "Nouvelle ligne. Merci, à bientôt."
10. "Annule ça, laisse tomber."

## 3. Domain / technical vocabulary passage

> "J'ai installé le snap myna-desktop avec whisper-snap et nemotron-snap,
> puis j'ai vérifié que PipeWire acheminait correctement mon microphone. Le
> raccourci déclenche l'injection IBus, et j'ai activé l'option preedit
> pour prévisualiser le texte instable avant sa validation. Après la mise à
> jour vers la version one point three point zero, j'ai vérifié la
> configuration à tilde slash dot config slash myna slash settings dot
> json pour m'assurer que le mode streaming était toujours réglé sur auto.
> L'extension GNOME Shell affiche l'indicateur d'activité sans voler le
> focus de mon terminal."

## 4. Numbers, dates, and punctuation-heavy passage

> "Appelle-moi au cinq cinq cinq, zéro un quatre deux, le vingt-neuf
> juillet deux mille vingt-six. Le montant total de la facture s'élevait à
> quatre cent douze dollars et cinquante cents, payable sous trente jours.
> Mon vol part à six heures quarante-cinq du matin, porte B douze, et le
> code de confirmation est X-Ray Tango quatre sept un."

## 5. Pangram / phonetic smoke-test

> "Portez ce vieux whisky au juge blond qui fume."

*(A standard, well-known French pangram, reused here for the full corpus —
same sentence as the TC-07 probe in `test-samples-en.md` §7.1.)*

## 6. Long continuous passage for streaming tests (30s+)

> "Des chercheurs ont annoncé cette semaine qu'un nouveau satellite
> météorologique a commencé à transmettre des données depuis l'orbite,
> fournissant aux prévisionnistes des images de plus haute résolution que
> les générations précédentes d'instruments. Le satellite, lancé plus tôt
> cette année, transporte des capteurs capables de suivre les systèmes de
> tempête en quasi temps réel, ce qui, selon les responsables, devrait
> améliorer les alertes précoces pour les communautés côtières. Pendant ce
> temps, les ingénieurs du centre de contrôle de la mission ont confirmé
> que tous les systèmes embarqués fonctionnaient dans les paramètres
> attendus, et que l'engin spatial a réussi sa première manœuvre
> d'ajustement orbital. La prochaine étape majeure, un étalonnage complet
> des instruments d'imagerie, devrait être achevée dans le mois à venir,
> après quoi le satellite commencera son service opérationnel de routine."
