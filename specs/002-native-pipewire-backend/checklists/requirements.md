# Specification Quality Checklist: Native PipeWire Capture Backend

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-07-15
**Feature**: [spec.md](../spec.md)

## Content Quality

- [x] No implementation details (languages, frameworks, APIs)
- [x] Focused on user value and business needs
- [x] Written for non-technical stakeholders
- [x] All mandatory sections completed

## Requirement Completeness

- [x] No [NEEDS CLARIFICATION] markers remain
- [x] Requirements are testable and unambiguous
- [x] Success criteria are measurable
- [x] Success criteria are technology-agnostic (no implementation details)
- [x] All acceptance scenarios are defined
- [x] Edge cases are identified
- [x] Scope is clearly bounded
- [x] Dependencies and assumptions identified

## Feature Readiness

- [x] All functional requirements have clear acceptance criteria
- [x] User scenarios cover primary flows
- [x] Feature meets measurable outcomes defined in Success Criteria
- [x] No implementation details leak into specification

## Notes

- Items marked incomplete require spec updates before `/speckit-clarify` or `/speckit-plan`
- Content Quality note: the spec deliberately names the *existing* reused components in
  conceptual terms (the capture core, bounded ring, stats tap, scripted fake fixture) because
  they are established seams the feature must not change; the technology-specific names
  ("pipewire-rs", "pw-record", trait/type identifiers) from the raw input are kept out of the
  requirements and success criteria and left to planning.
- Two points were resolved by clarification (Session 2026-07-15) and are no longer open: the
  subprocess backend is retired (FR-016), and device enumeration is live with change
  notifications (FR-008a, US4 scenario 3).
- **Implementation status (2026-07-15, `/speckit-implement`):** 33/38 tasks done, 5 partial.
  Native backend built + live-verified on real PipeWire (10 gated integration tests + channel
  unit tests; full workspace + clippy green). US1–US4 all functional. Remaining: one spoken
  transcript run (human voice) which also gates the `pw_record.rs` deletion (T033), kept last
  so `--mic` never breaks on `main`. One requirement (FR-004 absent-target fault) downgraded to
  a documented platform limitation — WirePlumber falls back to the default source for a bogus
  target (as `pw-record` did); positive selection by stable name is verified.
