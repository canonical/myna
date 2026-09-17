# Specification Quality Checklist: Accessible Dictation UX

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-08-26
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
- **Named technologies audit**: no assistive-technology product, accessibility bus, toolkit,
  or announcement mechanism is named in any requirement or success criterion. The
  requirements speak of "assistive technologies", "the accessibility path", and "the
  platform accessibility layer"; the concrete stack (screen reader, braille pipeline,
  accessibility-tree exposure) is deferred to planning and referenced only generically in
  Assumptions. WCAG 2.2 AA is named in Assumptions as the guiding *rubric* — a
  domain-standard acceptance reference, not an implementation choice — with its concrete
  thresholds restated in technology-neutral terms in FR-013 and FR-014 so the requirements
  remain independently testable.
- **Scope bounding**: three exclusions are stated with reasons rather than left implicit —
  text-injection accessibility (undo, edit-by-voice correction, in-field unstable
  hypotheses under assistive technology) deferred to its own feature per the 2026-08-26
  clarification; voice desktop control and wake word as pre-existing product non-goals; and
  development-only tools. The deferred injection risk is still recorded as an edge case so
  it is not lost, with an explicit gate on any future change that makes in-field hypotheses
  more prominent.
- **Overlap with existing backlog**: this feature intersects three open project-plan items —
  screen-reader announcements for the Shell indicator (T56), state-transition sound cues
  (T61), and the error-state UX mapping (T62), plus silence auto-stop (T59) and trigger
  alignment (T58). The Assumptions section states the ownership rule (this spec owns the
  accessibility requirements placed on them and delivers whatever is undelivered; it does
  not redefine the underlying wire error model), so the boundary is testable rather than
  ambiguous. Planning must reconcile task ownership across those items.
- **Testability of the qualitative criteria**: SC-001 and SC-007 are user-outcome
  measures rather than instrument readings. Each is bounded to make it verifiable — a
  stated condition (display off, non-visual channels only), a stated input restriction
  (no sighted assistance), and a stated time budget. Planning should name the participant
  count for these; the spec deliberately does not fix a sample size.
- **Constitution alignment**: FR-003 restates Principle V (no transcript content in any
  user-facing signal) as a hard constraint on the new announcement channel — the one place
  this feature could plausibly leak transcript text. FR-033 and FR-036 keep the acceptance
  gates runnable without a display or a human and against the confined package, satisfying
  Principle II's dual-environment requirement; FR-007's inert-when-unobserved clause exists
  so the new machinery cannot silently move a Principle III watermark.
- **2026-08-26 correction pass**: two issues raised in review, both fixed. (1) The original
  US2/FR-015/Assumptions text incorrectly framed tap-to-toggle as a product decision this
  spec needed to make; the codebase already defaults to toggle on both activation paths
  (`ActivationMode::Toggle` is `#[default]` in `client/myna-desktop/src/shortcut/portal.rs`,
  matched by the control-socket path). FR-015 is now stated as a non-regression requirement,
  US2's framing corrected, and FR-020 softened from implying Myna must build its own
  pointer-free control surface to requiring only that ordinary platform assistive-input
  events (switch access, dwell-click) work unmodified. (2) US4's original "settings surface"
  was flagged as likely scope creep — a bespoke standalone settings application is a new GUI
  surface the constitution would require in Rust, duplicating accessibility work the
  platform already provides. Narrowed to the GNOME Shell extension's standard preferences
  window, and the former US5 (first-run onboarding) was merged into it rather than kept as
  a second surface; stories renumbered 6→5, 7→6, 8→7 to close the resulting gap. The
  narrowing is recorded in Assumptions (not as a leaked implementation detail in a
  requirement) and a corresponding edge case documents the resulting limitation for desktops
  without the extension installed. Re-checked against all four checklist sections: still no
  named technologies in requirements (the preferences-window choice is confined to
  Assumptions, same treatment as GJS/D-Bus in feature 004); all changed requirements remain
  testable; success criteria SC-005/SC-009 updated to match and remain measurable and
  technology-agnostic. All items remain passing.
- **2026-08-26 second correction pass — settings UI removed from scope entirely.** After the
  narrowing above, a decision was made elsewhere that a settings/preferences UI (including
  first-run onboarding) is a separate feature owned by a different team. The former US4
  (preferences surface + first run) is removed outright — not narrowed further — along with
  its dedicated FR block (former FR-023–028) and its SC/edge-case references. Stories
  renumbered 5→4, 6→5, 7→6; FRs renumbered contiguously from FR-023 onward; SCs renumbered
  contiguously from SC-005 onward (no plan.md or tasks.md existed yet to hold stale
  references). Requirements that depend on a preference being configurable (FR-004
  announcement verbosity, FR-010 sound cues, FR-018/FR-021 auto-stop) are kept — they bind
  the preference's existence and effect, not its control surface — with a new Assumptions
  entry naming the dependency and the ownership split explicitly, so planning does not
  need to infer it. Re-checked: no implementation detail leaked (the dependency is stated
  as an out-of-scope item, not a design choice); all renumbered requirements remain
  testable and independently checkable; Success Criteria remain measurable and
  technology-agnostic after renumbering. All items remain passing.
- **2026-08-26 third pass \u2014 two remaining review findings addressed.** (1) *Cross-process
  preference consistency*: the preferences this spec depends on (verbosity, sound cues,
  auto-stop) are honoured by separate processes (desktop daemon, Shell extension, terminal
  client) with no shared store today. Added an edge case and extended the settings-UI
  Assumption to require that every consuming component observe the same value, and to
  cross-reference the already-open configuration-ownership boundary (`docs/project-plan.md`
  T54) rather than silently assume it resolves itself. No new design decision is made \u2014 the
  dependency is named so planning does not discover it late. (2) *Headless testing
  constraint*: FR-033 required automated, display-free verification of every indicator's
  accessibility properties, but GNOME Shell's nested-compositor mode is unavailable on
  Wayland, which may make full end-to-end AT-SPI-tree verification of the Shell HUD
  specifically infeasible without a real compositor session. FR-033 now requires the
  state-level accessible values be verified headlessly at minimum, with any end-to-end gap
  recorded rather than assumed closed; a matching edge case documents the platform
  constraint. Re-checked: both changes are stated as testable requirements/edge cases, not
  implementation prescriptions (no compositor-testing mechanism is named); no existing
  requirement was weakened, only qualified against a real platform limitation already
  documented elsewhere in the repo (feature 004's quickstart.md). All items remain passing.
- **2026-08-26 fourth pass — four small findings from a fresh read-through.** (1) SC-006 said
  "the reference corpus," a phrase this repo doesn't use elsewhere; corrected to "the real
  corpus" to match the existing fixture every other feature spec measures WER against
  (`corpus/real/`, `dev/fetch_real_corpus.py`). (2) US3's independent test exercised large
  text, high contrast, and reduced motion, but not forced colours, even though FR-012
  requires honouring it and an edge case names it — added it to the test so the requirement
  has matching coverage. (3) FR-021 bundled the secure-field acquisition wait (an internal,
  never-shown timing constant in `client/myna-desktop`) in with real user-facing timeouts
  under "time limits MUST be configurable"; WCAG 2.2.1 exempts essential non-interactive
  system timing, and there is no accessibility benefit to making that constant user-tunable.
  Narrowed FR-021 to the two actual user-facing timers (notice auto-dismiss, silence
  auto-stop) with an explicit exclusion clause. (4) Cognitive/learning support was thin —
  covered mostly by US4's plain-language failures — and thinner still after removing
  onboarding; added FR-024a requiring identical wording for the same concept across every
  surface (previously FR-024 only required consistent *meaning* for failures specifically,
  not consistent *naming* for states generally), a small, low-scope strengthener rather than
  a new story. Re-checked: no implementation detail introduced by any of the four; FR-021's
  narrowing and FR-024a are both independently testable; SC-006's correction is terminology
  only, no measurement change. All items remain passing.
- **2026-08-26 `/speckit-clarify` session — 3 questions asked and answered.** (1) *Default
  announcement verbosity* (FR-004): no default was stated, and since the settings UI to
  change it is a separate, unscheduled feature, an unhelpful default would make SC-001
  ("on first attempt") impossible to satisfy out of the box. Resolved to "all transitions"
  as the default; FR-004 now states it as a MUST. (2) *Error taxonomy dependency* (FR-023):
  the stable error-code taxonomy (T31) and error-state UX mapping (T62) are both `todo` in
  `docs/project-plan.md`, while FR-023 assumed a settled "product's error taxonomy" to map
  plain language onto. Initially resolved as a hard sequencing dependency (US4 blocked on
  T31/T62 landing first) — **reversed the same session** after review: US4 now maps plain
  language onto today's ad-hoc error strings (as originally written) and adopts T31/T62's
  failures compatibly if/when they land, matching the same soft-overlap treatment already
  given to T58/T59/T61 elsewhere in Assumptions, rather than singling this one out as a
  blocker. (3) *Announcement failure handling* (FR-002): unspecified what happens if
  emitting an announcement itself fails. Resolved as: the session is never affected, and
  the failure is itself surfaced as a recoverable notice (not silently swallowed) — added
  FR-002a and a matching edge case. All three answers are recorded in the Clarifications
  log (answer 2's log entry reflects the final, non-blocking resolution). Re-validated the
  checklist against the updated spec: no checkbox changed state (all items were already
  passing and remain so) — FR-002a and the reworded Assumptions bullet are both testable
  and free of implementation detail. 16/16 items passing, no regressions.
- **2026-08-26 fifth pass — localisation cut from scope entirely.** On review, US6 (every
  user-facing string translatable), FR-030–032, and SC-008 asked for RTL layout support and
  pseudo-translation CI gates ahead of any actual translation effort — no translation
  pipeline, translators, or locale roadmap exist for Myna. Unlike screen-reader support,
  which pays off immediately for users today, this only pays off once a second, unplanned
  workstream (real translation) also happens. Cut entirely rather than narrowed: removed
  User Story 6, its FR block (former FR-030–032), the "Locale with no translation" edge
  case, the localisation clause from the Input paragraph and from the surfaces-in-scope
  Clarification answer, and US4's cross-reference to "the localisation story". Renumbered
  contiguously: US7→US6 doesn't apply (US6 removed outright, nothing followed it — US5 is
  now last); the Verification FR block is now FR-030–033 (was FR-033–036); SC-008 (string
  scan) removed, SC-009/010 renumbered to SC-008/009. FR-032 (regression checks, was FR-035)
  had its "string extraction coverage" clause dropped since nothing extracts strings for
  translation now. Recorded as a new Clarification Q&A (matching the settings-UI removal's
  treatment) and a new out-of-scope Assumptions bullet explaining the rationale and noting
  the Shell extension's existing gettext scaffolding is unaffected and costs nothing to
  keep. Re-checked: no implementation detail introduced; all renumbered FRs/SCs remain
  testable and independently checkable. 16/16 items passing, no regressions.
