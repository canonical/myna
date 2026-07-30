# Myna Test Reading Sample Corpus (Portuguese / Português)

Portuguese (`pt`) translation of the reading sample corpus used by
`docs/test-plan-system.md`. Mirrors the structure of `docs/test-samples-en.md`
§1–§6.

> **Review status**: draft machine-assisted translation — **needs
> native/fluent Portuguese speaker review before use**, per
> `docs/test-plan-system.md` §2's requirement that accuracy judgments be
> made only by a fluent/native speaker of the language being tested. This
> draft is written in a broadly neutral Portuguese; confirm whether a
> European or Brazilian Portuguese variant should be preferred for actual
> testing.

**Product-specific terms convention**: product names, package names, and
technical identifiers (myna, whisper-snap, nemotron-snap, PipeWire, IBus,
Nemotron, GNOME Shell, version strings, file paths) are kept in **English**
throughout, matching how a real bilingual user would actually speak them —
these are not translated.

---

## 1. Natural long-form prose (2 passages)

**Passage A** (~35 seconds spoken)

> "A pesquisa em inteligência artificial tem se concentrado em alguns
> objetivos-chave: raciocínio, representação do conhecimento,
> planejamento, aprendizado, processamento de linguagem natural e
> percepção. A inteligência geral, a capacidade de realizar qualquer
> tarefa que um ser humano possa desempenhar, está entre os objetivos de
> longo prazo da área. Para alcançar essas metas, os pesquisadores
> utilizaram uma ampla gama de técnicas, incluindo busca e otimização
> matemática, lógica formal, redes neurais artificiais, e métodos
> baseados em estatística, probabilidade e economia."

**Passage B** (~30 seconds spoken)

> "Toda organização que utiliza computadores e redes enfrenta um conjunto
> básico de riscos de cibersegurança. Os funcionários podem ajudar a
> gerenciar esses riscos usando senhas fortes e exclusivas, mantendo o
> software atualizado, e tendo cuidado com anexos e links de remetentes
> desconhecidos. A autenticação multifator adiciona uma camada extra de
> proteção, mesmo que uma senha seja roubada. Fazer backup regularmente
> de arquivos importantes significa que um ataque de ransomware ou uma
> falha de hardware não precisam resultar em perda permanente de dados."

## 2. Command / short-utterance set

1. "Abra uma nova janela de terminal."
2. "Envie isso para a equipe até sexta-feira."
3. "Vírgula, novo parágrafo, ponto."
4. "Desfaça isso."
5. "Agende uma reunião para amanhã às três da tarde."
6. "Responda: combinado, até logo."
7. "Procure cafeterias próximas."
8. "Silencie o microfone."
9. "Nova linha. Obrigado, até breve."
10. "Cancele isso, deixa pra lá."

## 3. Domain / technical vocabulary passage

> "Instalei o snap myna-desktop junto com whisper-snap e nemotron-snap, e
> então confirmei que o PipeWire estava roteando corretamente meu
> microfone. O atalho de teclado aciona a injeção do IBus, e ativei a
> opção preedit para pré-visualizar o texto instável antes de ser
> confirmado. Depois de atualizar para a versão one point three point
> zero, verifiquei a configuração em tilde slash dot config slash myna
> slash settings dot json para garantir que o modo streaming ainda
> estivesse configurado como auto. A extensão do GNOME Shell mostra o
> indicador de atividade sem roubar o foco do meu terminal."

## 4. Numbers, dates, and punctuation-heavy passage

> "Me ligue no cinco cinco cinco, zero um quatro dois, no dia vinte e nove
> de julho de dois mil e vinte e seis. O total da fatura ficou em
> quatrocentos e doze dólares e cinquenta centavos, com vencimento em
> trinta dias. Meu voo sai às seis e quarenta e cinco da manhã pelo
> portão B doze, e o código de confirmação é X-Ray Tango quatro sete um."

## 5. Pangram / phonetic smoke-test

> "Um pequeno jabuti xereta viu dez cegonhas felizes."

*(A commonly used Portuguese pangram, used for phonetic density rather than
as a literal translation of the English fox pangram.)*

## 6. Long continuous passage for streaming tests (30s+)

> "Pesquisadores anunciaram esta semana que um novo satélite meteorológico
> começou a transmitir dados a partir da órbita, fornecendo aos
> meteorologistas imagens de resolução mais alta do que as gerações
> anteriores de instrumentos. O satélite, lançado no início deste ano,
> carrega sensores capazes de rastrear sistemas de tempestade quase em
> tempo real, o que, segundo autoridades, deve melhorar os alertas
> antecipados para comunidades costeiras. Enquanto isso, engenheiros do
> centro de controle da missão confirmaram que todos os sistemas de bordo
> estão operando dentro dos parâmetros esperados, e que a espaçonave
> completou com sucesso sua primeira manobra de ajuste orbital. O
> próximo grande marco, uma calibração completa dos instrumentos de
> imagem, deve ser concluído no próximo mês, após o qual o satélite
> iniciará seu serviço operacional de rotina."
