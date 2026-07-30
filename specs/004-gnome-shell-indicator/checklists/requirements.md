# Specification Quality Checklist: GNOME Shell Extension for Myna Dictation UI

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-07-21
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
- GJS/Clutter/St are named only in Assumptions as an unavoidable platform constraint of
  GNOME Shell extensions (an in-compositor UI cannot be Rust), flagged for Complexity
  Tracking in the plan — not as a design choice leaking into requirements. D-Bus is named
  as the integration boundary because the project's transport/IPC contract is part of the
  problem domain (feature 003 already exposes desktop boundaries), and the exact member
  signatures are deferred to planning.
- **2026-07-30 revision (HUD redesign)**: spec updated to replace the "goop" visual design
  with a bottom-center, OSD-styled pill; added the recoverable-vs-critical error severity
  distinction (US2a, FR-007/007a-c, FR-017) as an interim client-inferred classification
  pending T31/T62; added a segmented bar meter (FR-010) and contextual mic/mic-slash
  iconography in place of the continuous goop animation. Re-checked against all four
  checklist sections: no new implementation details leaked (St/Clutter/OsdWindow are named
  only in Assumptions as platform-constraint/architecture notes, same treatment as before);
  all new/changed requirements are testable; new success criteria (SC-002, SC-009) are
  measurable and technology-agnostic; acceptance scenarios and edge cases cover the new
  behavior. All items remain passing.
- **2026-07-30 clarify pass**: resolved 3 remaining ambiguities (concurrent critical-error
  handling → FR-007d; concurrent recoverable-issue handling → FR-007a restart-timer
  behavior; no coordination required with GNOME's native OSD → Assumptions). All 16
  checklist items already passing before this pass remain passing (16/16 → 16/16); no
  regressions, no newly-failing items. No further high-impact ambiguities identified.
- **2026-07-30 post-implementation (manual-test follow-up)**: two real bugs surfaced only
  in a live GNOME session — a level-update deduplication bug that made the VU meter go
  flat during steady speech, and an uncalibrated gain curve that needed shouting to move
  the meter. FR-010 tightened to require calibration to real speech (not raw linear gain);
  the "bar meter" language throughout was corrected to "segmented VU meter" with
  green/yellow/red colour zones (R16a). Documentation-only consolidation pass; no new
  ambiguities or ratified requirements changes beyond what FR-010's existing wording
  already covered. All 16 items remain passing.
