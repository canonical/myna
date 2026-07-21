// vumeter.js — PURE level → glow-intensity mapping with stale decay
// (feature 004, contract extension.md X5; research R5/R7).
//
// levelToGlow(level, ageMs) -> intensity in [0,1]: monotonic, clamped, and
// decaying to floor when the last level update is older than the stale window
// (~300 ms). Carries energy only — never samples, never content (privacy).
// Implementation lands with US3 (T026).
export {};
