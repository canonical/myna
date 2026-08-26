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
  signatures are deferred to planning. *(2026-08-26: this constraint now applies to the
  thin window-management host only — the renderer is a standalone Rust application; the
  Assumptions section reflects the shrunken carve-out.)*
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
- **2026-07-30 wave-ribbon redesign**: the segmented VU meter (US3, FR-010) is replaced
  with a flowing, accent-colored wave ribbon with distinct start/speaking/pause/stop
  behavior (new FR-010a-c), a reduced-motion fallback (new FR-022a), and two new Key
  Entities (accent-color preference, motion preference) sourced from the desktop
  environment rather than `myna-desktop`. Also records a non-shipped developer tuning
  tool as an Assumption/Out-of-Scope item — it has no independent functional requirements
  and carries no packaging obligations. Re-checked against all four checklist sections:
  no implementation details leaked (accent-color/reduced-motion are named only generically
  as "system preferences" in requirements, with the concrete GNOME mechanism deferred to
  planning, matching the existing D-Bus/GJS treatment); all new/changed requirements are
  testable (FR-010b's fallback rule, FR-010c's brightness cap, FR-022a's reduced-motion
  swap); new success criteria (SC-011, SC-012 measurable/tech-agnostic; SC-013 qualitative
  per the template's allowance for user-facing satisfaction measures, tightened to name an
  observer count and majority threshold for verifiability); US3's acceptance scenarios and
  two new edge cases cover the new behavior; scope bounds (no alternate meter style, no
  dev-tool packaging) added to Out of Scope. No `[NEEDS CLARIFICATION]` markers were
  needed — all open questions were resolved through direct discussion before this pass.
  All 16 items remain passing (16/16 → 16/16); no regressions.
- **2026-07-30 /speckit-analyze pass + remediation**: cross-artifact analysis (spec.md,
  plan.md, tasks.md, data-model.md, contracts/, quickstart.md) surfaced and fixed: (1) a
  missing FR/acceptance-scenario for the "complete" lifecycle phase that tasks.md/plan.md/
  research.md already assumed (new FR-010d, US3 acceptance scenario 9, contract X29); (2)
  SC-013 (the qualitative ribbon-vs-meter comparison) had no task operationalizing it (new
  T050a); (3) data-model.md's E2 entry still described the superseded "VU-meter" language
  despite the R17 rule beside it (retitled, Notes column corrected); (4) the original design
  brief's "aubergine if the main colour is orange" palette override had been generalized
  away in every derived artifact — reinstated in FR-010b, data-model E2a, contract X25, and
  task T054. All fixes re-verified against the four checklist sections: no implementation
  details leaked, all new/changed items testable and measurable, acceptance scenarios/edge
  cases cover the additions, scope remains bounded. All 16 items remain passing.
- **2026-07-30 "fabric in gentle airflow" refinement (post-implementation)**: a follow-up
  design pass on the built-and-tested wave ribbon (T051-T058a) found the first pass read as
  too literal/technical ("an oscilloscope"). Added FR-010 wording ruling that out explicitly;
  new FR-010e + SC-014 + US2a acceptance scenarios 6/7 + edge case for a genuine behavior
  change (confirmed with the product owner): the ribbon now stays visible/amber/paused-
  pulsing during a **recoverable** notice instead of hidden (a **critical** error still hides
  it). New contracts X30 (dots/convergence during morph/complete) and X31 (the
  recoverable-visible-amber guarantee). An Assumption records that sparse particle
  highlights — an optional 4th layer in the design brief — are detection-only in this pass,
  not rendered, a deliberate scope cut the brief itself invites ("too many would look like a
  music visualizer"). Re-checked against all four sections: no implementation details leaked
  (the ~300ms smoothing constant and layered-strand structure are documented in
  plan.md/research.md, not the requirements); all new/changed requirements testable
  (`ribbon.test.js`/`hud.test.js` cover the pure logic; the visual result is manual-acceptance
  per the existing harness-tier split); SC-014 measurable/technology-agnostic; scope
  (particle deferral, behavior-change confirmation) explicitly bounded. All 16 items remain
  passing.
- **2026-08-26 architecture revision (renderer → GTK app; extension → thin host)**: spec
  updated with the 2026-08-26 clarification session (9 Q&As) and the derived changes:
  new FR-024–FR-027 (overlay-window behavior, click-through input region, supervision,
  well-known binary), FR-017a (`org.myna.Shell` presence), FR-016/018 consumer re-homing,
  FR-021 teardown of the hosted process, FR-023 fallback selection + `ui-gtk` removal;
  three new Key Entities (Shell presence, Hosted overlay window, Renderer application);
  new SC-015/SC-016; edge cases for renderer crash/extension disable; Assumptions rewritten
  (renderer application, supervision model, well-known binary, localization under the
  `myna` domain, lab/simulator inside the binary); Out of Scope updated (contract-only
  non-GNOME path; rasterizer choice removed). Re-checked against all four sections: the
  renderer application and GTK are named where they define *what* the system is (the
  deliverable shape), not how to build it — the same treatment the checklist already grants
  D-Bus/GJS as platform boundaries; FR-024–FR-027 are testable user-observable window
  behaviors (no window-list entry, all workspaces, above normal windows, click-through,
  auto-restart); SC-015/SC-016 measurable and technology-agnostic; acceptance scenarios and
  edge cases cover the new failure modes. All 16 items remain passing (16/16 → 16/16); no
  regressions, no `[NEEDS CLARIFICATION]` markers (all questions resolved in the session).
