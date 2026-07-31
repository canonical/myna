# Contract: HUD Style Preference

**Feature**: 009-switchable-basic-hud | **Date**: 2026-07-31

This is the user-visible persistent configuration contract for the GNOME Shell
extension. It is local to the extension and does not alter
`org.myna.Dictation`.

## Schema

| Item | Contract |
|---|---|
| Schema ID | `org.gnome.shell.extensions.myna` |
| Schema path | `/org/gnome/shell/extensions/myna/` |
| Key | `hud-style` |
| Type | closed enum |
| Values | `basic` (numeric 0), `wave` (numeric 1) |
| Default | `basic` |
| Persistence | per-user GSettings backend |

Numeric values and nicks are stable once released. Missing, invalid, or unknown
values resolve to `basic` at the application boundary without a user-facing
error.

## Preferences UI guarantees

| # | Guarantee | Spec |
|---|---|---|
| S1 | The extension preferences expose one clearly labelled HUD-style selector with Basic and Wave ribbon choices. | FR-001/002 |
| S2 | Opening preferences reflects the currently persisted choice. | FR-003 |
| S3 | Selecting a choice persists it immediately; no Apply button or Shell restart is required. | FR-003/006 |
| S4 | The preferences process imports GTK4/libadwaita and settings APIs only, never Shell compositor modules. | platform constraint |
| S5 | No preference or label contains transcript content, audio data, meter sensitivity, colour, geometry, model, microphone, or language configuration. | FR-023, scope |

## Shell-process guarantees

| # | Guarantee | Spec |
|---|---|---|
| S6 | The settings change is observed live and applied within 250 ms on reference hardware. | SC-002 |
| S7 | Reading or changing the preference while idle does not make a HUD visible. | FR-007 |
| S8 | Re-enabling the extension or starting a new desktop session restores the last valid choice. | FR-003, SC-003 |
| S9 | Disable disconnects the settings handler; later changes cannot call retired extension/controller instances. | FR-008 |

## Packaging guarantees

| # | Guarantee |
|---|---|
| S10 | `metadata.json` declares the schema ID. |
| S11 | The source/package includes `schemas/org.gnome.shell.extensions.myna.gschema.xml`. |
| S12 | The packaged extension passes `gnome-extensions pack` and installation compiles its local schema. |
| S13 | Manual raw-copy installation compiles the destination's local schema before enabling the extension. |
| S14 | Generated `gschemas.compiled` is not a source-controlled artifact. |

## Compatibility

The schema is additive to the existing extension bundle. It has no D-Bus,
protocol-version, snap, or server compatibility effect. Future HUD styles require
an explicit new enum value and updated fallback/tests; unknown future values in
an older build still resolve safely to `basic`.
