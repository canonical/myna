# Architecture Decisions

This directory contains the architecture decision records for the Ubuntu Speech to Text (Myna) project.

Each file captures a significant architectural choice: its context, what was decided, why, and the consequences. Reading these records before making structural decisions prevents the team (and agents) from relitigating settled questions.

These ADRs are maintained as the current minimal accepted version of each architectural topic. If a covered topic evolves, update that ADR in place instead of creating a superseding follow-up ADR for the same subject.

## How to use

**Before making a major architectural decision**, read through the index below and the relevant decision files so you understand what has already been decided and why.

**After making a major architectural decision**:

1. If the topic is already covered by an existing ADR, update that file directly so it reflects the current accepted state.
2. If the topic is new, copy `docs/architecture/template.md` to `docs/architecture/kebab-case-topic.md`.
3. Fill in every section.
4. Add an entry to the index below only when you created a new ADR file.

## What counts as a major architectural decision

- Module layout or structure choices that affect the whole codebase
- Choice of abstraction pattern (trait objects vs generics, etc.)
- Async/sync boundary decisions
- Transport choices (stdio vs Unix socket, etc.)
- Error handling strategy
- Cross-cutting conventions that affect multiple modules

Routine implementation details (adding a field, renaming a variable, choosing a module structure for one server) do not need a decision file.

## Documentation boundaries

Two directories hold architecture artefacts; they have different lifecycles:

| Location | What lives here | Lifecycle |
| --- | --- | --- |
| `docs/architecture/` | **ADRs** — durable decisions (context, decision, rationale, consequences). Updated in place as a topic evolves. Architecture diagrams that must remain true long-term belong here, as validated Mermaid (not opaque PNGs). | Live — update in place |
| `specs/NNN-*/` | **Spec kits** — per-feature artefacts (spec, plan, tasks, contracts, data model, diagrams). A snapshot of intent at the time the feature was designed. | Frozen at merge — do not update |

Rule: *specs freeze at merge; ADRs live.* If a spec-kit diagram captures something that must stay true beyond that feature's scope, promote it into an ADR as a Mermaid block.

All Mermaid diagrams in `docs/architecture/` are validated with `mmdc` (`@mermaid-js/mermaid-cli`) before commit. See the Mermaid validation notes in `CLAUDE.md` for gotchas.

## Index

| Topic | Title | Status | Date |
| --- | --- | --- | --- |
| [Ubuntu Desktop STT Integration Specification](UD129 - Ubuntu Desktop STT Integration .md) | Initial High Level Specification | In Review | 2026-06-11 |
| [Repository and Module Layout](repository-layout.md) | Python package + Rust workspace + snaps | Accepted | 2026-07-19 |
| [Transport Abstraction and Event Vocabulary](transport-and-events.md) | Session contract over in-flux transport | Accepted (details provisional) | 2026-06-12 |
