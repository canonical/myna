//! shader — GENERATES the wave ribbon's GLSL fragment shader from the shared
//! tuning constants (feature 004, 2026-08-21 GPU rasterization pass; ported
//! from `extensions/myna-shell/ribbonGlsl.js` + the shared tables of
//! `ribbonPaint.js`, which are now the single source of truth — R23: the
//! Cairo painter itself is deliberately NOT ported, GPU-only rendering).
//!
//! # Why a generator rather than a .glsl file
//!
//! The tuning tables (gradient stops, glow/feather pass tables, the billow
//! and taper shapes, the per-role thicknesses) are exactly where two
//! hand-maintained copies would silently drift apart — the same class of bug
//! [`compute_safe_scale`] was written to close. So this module reads the
//! constants from the one place they are defined and bakes them into the
//! shader source as `#define`s and unrolled expressions. There is no build
//! step: [`glsl_constant_defines`] returns a string, so a retune of either
//! side cannot quietly desynchronize them — [`tests/shader.rs`] asserts the
//! emitted `#define`s still equal their Rust originals.
//!
//! # What is NOT in the shader
//!
//! The model itself. [`crate::ribbon::compute_ribbon_model`] (phase state
//! machine, envelope smoothing, the amplitude response curve) stays pure,
//! stays headlessly testable, and stays the single authority for *what* to
//! draw. The shader only rasterizes, and it regenerates each strand's sine
//! analytically from the per-strand parameters the model reports
//! (`amplitude`, `phase_offset`, `delay_ms`, `speed_scale`) rather than from
//! constants of its own. Because the centreline is single-valued
//! (`y = f(x)`), no polyline SDF is needed — the vertical distance IS the
//! distance.
//!
//! # Profiles
//!
//! The generated core is written in Cogl style (`cogl_tex_coord_in[0]`,
//! `cogl_color_out`), matching the original. [`standalone_shader`] wraps it
//! into a complete, compilable shader for a target GL profile (desktop GL
//! 1.20 / ES 1.00 / ES 3.00 — GLArea's context is GLES on Wayland), the way
//! the former Python GPU lab's `ribbon_gl.py` did.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use crate::ribbon::{
    RibbonModel, RibbonTint, StrandRole, DEFAULT_STRAND_COUNT, FLOW_SPEED, SPATIAL_FREQUENCY,
};

// ── Shared tuning tables (ribbonPaint.js's tables — the Cairo painter's
// ── drawing code is not ported; its TUNING is the shader's contract).

/// The amber palette used when a recoverable notice tints the ribbon.
pub const AMBER_MAIN: &str = "#F5A623";
pub const AMBER_HIGHLIGHT: &str = "#FFE0A6";

/// Paint's own per-role thickness — referenced by [`compute_safe_scale`] so
/// the overflow guard stays honest if the body's billow is ever retuned.
pub const VOICE_THICKNESS_FRACTION: f64 = 0.85;
/// The un-clamped verticalScale factor.
pub const BASE_CENTRELINE_FRACTION: f64 = 0.82;
/// `body_thickness`'s ceiling (activity = 1).
pub const MAX_BODY_BILLOW: f64 = 1.0 + 0.12 + 0.32;
/// The wisp thickness (scales with safe_scale; not part of its budget).
pub const WISP_THICKNESS_FRACTION: f64 = 0.5;

/// Per-role body thickness, as a fraction of the canvas height.
pub fn role_thickness_fraction(role: StrandRole) -> f64 {
    match role {
        StrandRole::Voice => VOICE_THICKNESS_FRACTION,
        StrandRole::Secondary => 0.4,
        StrandRole::Base => 0.32,
    }
}

/// Per-role extra alpha multiplier (the base strand reads as soft haze).
pub fn role_alpha_scale(role: StrandRole) -> f64 {
    match role {
        StrandRole::Voice | StrandRole::Secondary => 1.0,
        StrandRole::Base => 0.5,
    }
}

/// A gradient stop's tone, indexing the resolved palette rather than naming
/// a colour, so the amber-tint and accent-coloured paths reuse the identical
/// stop positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientTone {
    Shadow,
    Main,
    Highlight,
}

/// One stop of a left→right gradient: position, tone, alpha.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub pos: f64,
    pub tone: GradientTone,
    pub alpha: f64,
}

/// The left→right ribbon gradient: shifts tone (shadow → main → highlight →
/// main → shadow) while fading alpha in/out at the ends.
pub const RIBBON_GRADIENT_STOPS: [GradientStop; 7] = [
    GradientStop {
        pos: 0.0,
        tone: GradientTone::Shadow,
        alpha: 0.0,
    },
    GradientStop {
        pos: 0.08,
        tone: GradientTone::Shadow,
        alpha: 0.45,
    },
    GradientStop {
        pos: 0.32,
        tone: GradientTone::Main,
        alpha: 0.9,
    },
    GradientStop {
        pos: 0.52,
        tone: GradientTone::Highlight,
        alpha: 0.95,
    },
    GradientStop {
        pos: 0.72,
        tone: GradientTone::Main,
        alpha: 0.85,
    },
    GradientStop {
        pos: 0.92,
        tone: GradientTone::Shadow,
        alpha: 0.35,
    },
    GradientStop {
        pos: 1.0,
        tone: GradientTone::Shadow,
        alpha: 0.0,
    },
];

/// One pass of a glow/feather stack: progressively wider, fainter strokes;
/// the shader evaluates the same table as that many summed Gaussians.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GaussianPass {
    pub scale: f64,
    pub alpha_scale: f64,
}

/// The glow passes (a fake bloom — the stack these approximate).
pub const GLOW_PASSES: [GaussianPass; 3] = [
    GaussianPass {
        scale: 4.6,
        alpha_scale: 0.10,
    },
    GaussianPass {
        scale: 2.8,
        alpha_scale: 0.17,
    },
    GaussianPass {
        scale: 1.6,
        alpha_scale: 0.24,
    },
];

/// The body-edge feathering passes (same idea, applied to each edge curve).
pub const FEATHER_PASSES: [GaussianPass; 2] = [
    GaussianPass {
        scale: 2.2,
        alpha_scale: 0.12,
    },
    GaussianPass {
        scale: 1.3,
        alpha_scale: 0.18,
    },
];

/// One wisp tendril curling off the voice strand.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WispTendril {
    pub seed: f64,
    pub alpha: f64,
    pub time_offset_ms: f64,
    pub mix: f64,
    pub from_shadow: bool,
}

/// The two wisp tendrils.
pub const WISP_TENDRILS: [WispTendril; 2] = [
    WispTendril {
        seed: 0.7,
        alpha: 0.5,
        time_offset_ms: 0.0,
        mix: 0.4,
        from_shadow: false,
    },
    WispTendril {
        seed: 1.9,
        alpha: 0.35,
        time_offset_ms: 240.0,
        mix: 0.5,
        from_shadow: true,
    },
];

/// The wisp's own left→right alpha gradient (fades in and out along x).
pub const WISP_GRADIENT_STOPS: [GradientStop; 4] = [
    GradientStop {
        pos: 0.0,
        tone: GradientTone::Main,
        alpha: 0.0,
    },
    GradientStop {
        pos: 0.25,
        tone: GradientTone::Main,
        alpha: 0.5,
    },
    GradientStop {
        pos: 0.55,
        tone: GradientTone::Main,
        alpha: 0.32,
    },
    GradientStop {
        pos: 1.0,
        tone: GradientTone::Main,
        alpha: 0.0,
    },
];

/// The wisp tendril's curl magnitude, drift wave and stroke width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wisp {
    pub curl_min: f64,
    pub curl_activity: f64,
    pub tail_floor: f64,
    pub freq_base: f64,
    pub freq_seed: f64,
    pub speed_base: f64,
    pub speed_seed: f64,
    pub phase_seed: f64,
    pub alpha_min: f64,
    pub alpha_activity: f64,
    pub line_width_fraction: f64,
}

pub const WISP: Wisp = Wisp {
    curl_min: 0.12,
    curl_activity: 1.5,
    tail_floor: 0.25,
    freq_base: 4.4,
    freq_seed: 1.3,
    speed_base: 0.0017,
    speed_seed: 0.0004,
    phase_seed: 2.1,
    alpha_min: 0.12,
    alpha_activity: 0.88,
    line_width_fraction: 0.16,
};

/// `body_thickness`'s billow shape, and the drift wave that produces it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Billow {
    pub min_amount: f64,
    pub activity_amount: f64,
    pub freq: f64,
    pub speed: f64,
    pub phase: f64,
    pub taper_floor: f64,
}

pub const BILLOW: Billow = Billow {
    min_amount: 0.12,
    activity_amount: 0.32,
    freq: 2.3,
    speed: 0.0009,
    phase: 0.6,
    taper_floor: 0.32,
};

/// `edge_taper`'s raised-cosine in/out widths.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeTaper {
    pub in_width: f64,
    pub out_width: f64,
}

pub const EDGE_TAPER: EdgeTaper = EdgeTaper {
    in_width: 0.16,
    out_width: 0.3,
};

/// `activity_ramp`'s smoothstep bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActivityRamp {
    pub lo: f64,
    pub hi: f64,
}

pub const ACTIVITY_RAMP: ActivityRamp = ActivityRamp { lo: 0.08, hi: 0.3 };

/// How much intentional overflow to allow beyond the guaranteed-no-clip
/// budget (`1` keeps the original guarantee; `> 1` relaxes it proportionally,
/// trading some of that guarantee for a bigger, more dramatic wave that may
/// occasionally graze or clip at extreme, sustained loudness). Capped at
/// `min(1, …)` in [`compute_safe_scale`] regardless.
pub const OVERFLOW_BOOST: f64 = 1.3;

/// The overflow guard (2026-07-31 cropping-bug fix, relaxed via
/// [`OVERFLOW_BOOST`]): derives the scale factor from the SAME ceilings the
/// body's billow actually reaches, so a retune cannot silently drift it out
/// of sync. Returns `1` (no shrink) once the boosted budget already covers
/// the worst case.
///
/// Port of `ribbonPaint.js`'s `computeSafeScale`.
pub fn compute_safe_scale() -> f64 {
    let worst_case_extent_fraction =
        BASE_CENTRELINE_FRACTION + (VOICE_THICKNESS_FRACTION * MAX_BODY_BILLOW) / 2.0;
    (0.5 * OVERFLOW_BOOST / worst_case_extent_fraction).min(1.0)
}

fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

/// Smoothstep ramp in `[0,1]`: 0 at/below `lo`, 1 at/above `hi`, smooth in
/// between (no kink) — used to fade the glow/feather/wisp effects in as
/// activity rises, rather than a hard on/off that would visibly "pop".
///
/// Port of `ribbonPaint.js`'s `activityRamp`.
pub fn activity_ramp(activity: f64) -> f64 {
    activity_ramp_bounds(activity, ACTIVITY_RAMP.lo, ACTIVITY_RAMP.hi)
}

pub fn activity_ramp_bounds(activity: f64, lo: f64, hi: f64) -> f64 {
    if activity <= lo {
        return 0.0;
    }
    if activity >= hi {
        return 1.0;
    }
    let t = (activity - lo) / (hi - lo);
    t * t * (3.0 - 2.0 * t)
}

// ── Colours ───────────────────────────────────────────────────────────────

/// An RGB colour as 0..=1 floats (the shader's uniform space).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

/// Parse a `#rrggbb` hex string; anything else degrades to white (the GJS
/// `colorToRgbFloat` string path, kept so a bad palette string can never
/// panic the renderer).
pub fn hex_to_rgb(color: &str) -> Rgb {
    let t = color.trim();
    let hex = t.strip_prefix('#').unwrap_or(t);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Rgb {
            r: 1.0,
            g: 1.0,
            b: 1.0,
        };
    }
    let n = u32::from_str_radix(hex, 16).unwrap_or(0xffffff);
    Rgb {
        r: ((n >> 16) & 0xff) as f64 / 255.0,
        g: ((n >> 8) & 0xff) as f64 / 255.0,
        b: (n & 0xff) as f64 / 255.0,
    }
}

fn mix_rgb(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = clamp01(t);
    Rgb {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
    }
}

fn darken_rgb(c: Rgb, amount: f64) -> Rgb {
    let a = clamp01(amount);
    Rgb {
        r: c.r * (1.0 - a),
        g: c.g * (1.0 - a),
        b: c.b * (1.0 - a),
    }
}

/// The caller-resolved ribbon palette (from the accent module; hex strings
/// parse through [`hex_to_rgb`] so tests and the lab can pass literals).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RibbonPalette {
    pub main: Rgb,
    pub highlight: Rgb,
    pub darker_complement: Rgb,
    pub translucent_alpha: f64,
}

impl RibbonPalette {
    /// From `#rrggbb` hex strings (the GJS test/literal shape).
    pub fn from_hex(
        main: &str,
        highlight: &str,
        darker_complement: &str,
        translucent_alpha: f64,
    ) -> Self {
        Self {
            main: hex_to_rgb(main),
            highlight: hex_to_rgb(highlight),
            darker_complement: hex_to_rgb(darker_complement),
            translucent_alpha,
        }
    }
}

/// The palette + activity state the shader consumes, resolved from the model
/// (amber tint override included).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedPalette {
    pub main_rgb: Rgb,
    pub highlight_rgb: Rgb,
    pub shadow_rgb: Rgb,
    /// How much audio is coming through right now, derived from the voice
    /// strand's own amplitude (~0 at idle, ~1 at loud).
    pub activity: f64,
    /// A smoothed 0-1 ramp of `activity` used to fade the glow/feather/wisp
    /// embellishments in/out.
    pub effect_strength: f64,
}

/// Resolve the model + palette into the shader's colour/activity state: the
/// amber tint overrides main/highlight/complement during a recoverable
/// notice; the shadow tone is the complement blended 60% toward the main
/// colour and darkened, so it reads as a warm undertone rather than a bold
/// second hue.
///
/// Port of `ribbonPaint.js`'s `resolveRibbonPalette`.
pub fn resolve_ribbon_palette(model: &RibbonModel, palette: &RibbonPalette) -> ResolvedPalette {
    let amber = model.tint == Some(RibbonTint::Amber);
    let main_rgb = if amber {
        hex_to_rgb(AMBER_MAIN)
    } else {
        palette.main
    };
    let highlight_rgb = if amber {
        hex_to_rgb(AMBER_HIGHLIGHT)
    } else {
        palette.highlight
    };
    let complement_rgb = if amber {
        hex_to_rgb(AMBER_MAIN)
    } else {
        palette.darker_complement
    };
    let shadow_rgb = darken_rgb(mix_rgb(complement_rgb, main_rgb, 0.6), 0.45);

    let activity = model
        .strands
        .iter()
        .find(|s| s.role == StrandRole::Voice)
        .map(|voice| voice.points.iter().map(|p| p.y.abs()).fold(0.0, f64::max))
        .map(clamp01)
        .unwrap_or(1.0);
    ResolvedPalette {
        main_rgb,
        highlight_rgb,
        shadow_rgb,
        activity,
        effect_strength: activity_ramp(activity),
    }
}

// ── Uniform layout ────────────────────────────────────────────────────────

/// Upper bound on the number of strands the shader can draw (GLSL needs a
/// compile-time count; the model never returns more than this).
pub const MAX_STRANDS: usize = DEFAULT_STRAND_COUNT;

/// How many travelling dots the `morph` phase produces. Packed into a single
/// vec3, so this cannot exceed 4 without repacking.
pub const MAX_DOTS: usize = 3;

/// Numeric role tags, since GLSL has no strings.
pub fn role_tag(role: StrandRole) -> i32 {
    match role {
        StrandRole::Voice => 0,
        StrandRole::Secondary => 1,
        StrandRole::Base => 2,
    }
}

/// Painter's order, back to front: base (soft haze) → secondary (shadow
/// depth) → voice (the bright focal strand). The uniform upload sorts by it
/// so the shader can composite strand 0, 1, 2… in index order.
pub const PAINT_ORDER: [StrandRole; 3] =
    [StrandRole::Base, StrandRole::Secondary, StrandRole::Voice];

/// One declared uniform: its name and component count. All of them are
/// scalars or vec2/3/4 — never arrays (the historical Cogl
/// `ClutterShaderFloat` marshalling asserted `size <= 4`; the packing is kept
/// because it is also what fits GLES uniform upload cleanly).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UniformSpec {
    pub name: &'static str,
    pub components: usize,
}

/// Every uniform the generated shader declares, with its component count.
/// The GLArea renderer uploads exactly this set; `tests/shader.rs` asserts
/// the two agree, so a uniform cannot be added to the shader and forgotten
/// in the uploader.
pub fn ribbon_uniforms() -> &'static [UniformSpec] {
    static SPECS: std::sync::OnceLock<Vec<UniformSpec>> = std::sync::OnceLock::new();
    SPECS.get_or_init(|| {
        let mut specs = vec![
            UniformSpec {
                name: "uSize",
                components: 2,
            },
            UniformSpec {
                name: "uElapsedMs",
                components: 1,
            },
            UniformSpec {
                name: "uActivity",
                components: 1,
            },
            UniformSpec {
                name: "uEffectStrength",
                components: 1,
            },
            UniformSpec {
                name: "uBrightnessBoost",
                components: 1,
            },
        ];
        for i in 0..MAX_STRANDS {
            specs.push(UniformSpec {
                name: Box::leak(format!("uStrandGeom{i}").into_boxed_str()),
                components: 4,
            });
            specs.push(UniformSpec {
                name: Box::leak(format!("uStrandStyle{i}").into_boxed_str()),
                components: 3,
            });
        }
        specs.extend([
            UniformSpec {
                name: "uVoice",
                components: 4,
            },
            UniformSpec {
                name: "uMain",
                components: 3,
            },
            UniformSpec {
                name: "uHighlight",
                components: 3,
            },
            UniformSpec {
                name: "uShadow",
                components: 3,
            },
            UniformSpec {
                name: "uDotX",
                components: MAX_DOTS,
            },
            UniformSpec {
                name: "uDotAlpha",
                components: 1,
            },
            UniformSpec {
                name: "uConvergence",
                components: 3,
            },
        ]);
        specs
    })
}

/// Pack a ribbon model into the shader's uniform values. Pure — no GL — so
/// the GLArea renderer, the headless render test and the lab all upload the
/// *same* numbers rather than three hand-copied packings. That matters more
/// than it looks: the packing encodes the paint order and the role tags, so
/// a second copy is exactly where the renderers would silently diverge.
///
/// Port of `ribbonGlsl.js`'s `packRibbonUniforms`.
///
/// Returns uniform name → exactly `components` values, one entry per
/// [`ribbon_uniforms`] member.
pub fn pack_ribbon_uniforms(
    width: f64,
    height: f64,
    model: &RibbonModel,
    palette: &RibbonPalette,
) -> BTreeMap<String, Vec<f32>> {
    let resolved = resolve_ribbon_palette(model, palette);
    let mut values: BTreeMap<String, Vec<f32>> = BTreeMap::new();

    let ins = |v: f64| v as f32;
    values.insert("uSize".into(), vec![ins(width), ins(height)]);
    values.insert("uElapsedMs".into(), vec![ins(model.elapsed_ms)]);
    values.insert("uActivity".into(), vec![ins(resolved.activity)]);
    values.insert(
        "uEffectStrength".into(),
        vec![ins(resolved.effect_strength)],
    );
    values.insert("uBrightnessBoost".into(), vec![ins(model.brightness_boost)]);
    values.insert(
        "uMain".into(),
        vec![
            ins(resolved.main_rgb.r),
            ins(resolved.main_rgb.g),
            ins(resolved.main_rgb.b),
        ],
    );
    values.insert(
        "uHighlight".into(),
        vec![
            ins(resolved.highlight_rgb.r),
            ins(resolved.highlight_rgb.g),
            ins(resolved.highlight_rgb.b),
        ],
    );
    values.insert(
        "uShadow".into(),
        vec![
            ins(resolved.shadow_rgb.r),
            ins(resolved.shadow_rgb.g),
            ins(resolved.shadow_rgb.b),
        ],
    );

    // Sorted back-to-front here rather than in the shader: the painter's
    // order is a CPU-side fact, and pre-sorting lets the shader composite
    // strand 0, 1, 2… in index order. The model never returns more strands
    // than the shader has slots, but clamping keeps a future strand-count
    // bump from silently dropping off the end unnoticed.
    let ordered: Vec<&crate::ribbon::Strand> = PAINT_ORDER
        .iter()
        .flat_map(|role| model.strands.iter().filter(move |s| s.role == *role))
        .take(MAX_STRANDS)
        .collect();

    for i in 0..MAX_STRANDS {
        let strand = ordered.get(i).copied();
        values.insert(
            format!("uStrandGeom{i}"),
            strand
                .map(|s| {
                    vec![
                        ins(s.amplitude),
                        ins(s.phase_offset),
                        ins(s.delay_ms),
                        ins(s.speed_scale),
                    ]
                })
                .unwrap_or_else(|| vec![0.0; 4]),
        );
        // The third component is the "active" flag: an absent strand is
        // skipped outright rather than drawn at zero alpha, so it costs
        // nothing and can never contribute a stray pixel.
        values.insert(
            format!("uStrandStyle{i}"),
            strand
                .map(|s| vec![ins(s.alpha), ins(role_tag(s.role) as f64), 1.0])
                .unwrap_or_else(|| vec![0.0; 3]),
        );
    }

    // The wisps curl off the voice strand specifically, so its parameters
    // are uploaded separately rather than searched for in the shader.
    let voice = model.strands.iter().find(|s| s.role == StrandRole::Voice);
    values.insert(
        "uVoice".into(),
        voice
            .map(|s| {
                vec![
                    ins(s.amplitude),
                    ins(s.phase_offset),
                    ins(s.delay_ms),
                    ins(s.speed_scale),
                ]
            })
            .unwrap_or_else(|| vec![0.0; 4]),
    );

    let dot_list = model.dots.as_deref().unwrap_or(&[]);
    values.insert(
        "uDotX".into(),
        (0..MAX_DOTS)
            .map(|i| dot_list.get(i).map(|d| ins(d.x)).unwrap_or(0.0))
            .collect(),
    );
    values.insert(
        "uDotAlpha".into(),
        vec![dot_list.first().map(|d| ins(d.alpha)).unwrap_or(0.0)],
    );

    values.insert(
        "uConvergence".into(),
        model
            .convergence
            .map(|c| vec![ins(c.x), ins(c.y), ins(c.alpha)])
            .unwrap_or_else(|| vec![0.0; 3]),
    );

    values
}

// ── Shader generation ─────────────────────────────────────────────────────

/// Emit an f64 as a GLSL float literal (GLSL has no implicit int→float
/// conversion in expressions like `1 / 2`, so integers need a decimal).
/// Panics on non-finite input — a generator refusing to emit garbage, the
/// same contract the GJS `f()` exception enforced.
fn f(value: f64) -> String {
    if !value.is_finite() {
        panic!("ribbon shader: refusing to emit non-finite {value}");
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

/// The `#define` block: every shared constant, named after its Rust origin so
/// a grep for the constant name finds the shader's copy too.
///
/// Port of `ribbonGlsl.js`'s `glslConstantDefines`.
pub fn glsl_constant_defines() -> String {
    let defines: [(&str, f64); 33] = [
        ("MYNA_PI", PI),
        ("MYNA_SPATIAL_FREQUENCY", SPATIAL_FREQUENCY),
        ("MYNA_FLOW_SPEED", FLOW_SPEED),
        ("MYNA_BASE_CENTRELINE_FRACTION", BASE_CENTRELINE_FRACTION),
        ("MYNA_SAFE_SCALE", compute_safe_scale()),
        ("MYNA_TAPER_IN", EDGE_TAPER.in_width),
        ("MYNA_TAPER_OUT", EDGE_TAPER.out_width),
        ("MYNA_BILLOW_MIN", BILLOW.min_amount),
        ("MYNA_BILLOW_ACTIVITY", BILLOW.activity_amount),
        ("MYNA_BILLOW_FREQ", BILLOW.freq),
        ("MYNA_BILLOW_SPEED", BILLOW.speed),
        ("MYNA_BILLOW_PHASE", BILLOW.phase),
        ("MYNA_TAPER_FLOOR", BILLOW.taper_floor),
        ("MYNA_ACTIVITY_LO", ACTIVITY_RAMP.lo),
        ("MYNA_ACTIVITY_HI", ACTIVITY_RAMP.hi),
        (
            "MYNA_THICKNESS_VOICE",
            role_thickness_fraction(StrandRole::Voice),
        ),
        (
            "MYNA_THICKNESS_SECONDARY",
            role_thickness_fraction(StrandRole::Secondary),
        ),
        (
            "MYNA_THICKNESS_BASE",
            role_thickness_fraction(StrandRole::Base),
        ),
        ("MYNA_ALPHA_VOICE", role_alpha_scale(StrandRole::Voice)),
        (
            "MYNA_ALPHA_SECONDARY",
            role_alpha_scale(StrandRole::Secondary),
        ),
        ("MYNA_ALPHA_BASE", role_alpha_scale(StrandRole::Base)),
        ("MYNA_WISP_THICKNESS_FRACTION", WISP_THICKNESS_FRACTION),
        ("MYNA_WISP_CURL_MIN", WISP.curl_min),
        ("MYNA_WISP_CURL_ACTIVITY", WISP.curl_activity),
        ("MYNA_WISP_TAIL_FLOOR", WISP.tail_floor),
        ("MYNA_WISP_FREQ_BASE", WISP.freq_base),
        ("MYNA_WISP_FREQ_SEED", WISP.freq_seed),
        ("MYNA_WISP_SPEED_BASE", WISP.speed_base),
        ("MYNA_WISP_SPEED_SEED", WISP.speed_seed),
        ("MYNA_WISP_PHASE_SEED", WISP.phase_seed),
        ("MYNA_WISP_ALPHA_MIN", WISP.alpha_min),
        ("MYNA_WISP_ALPHA_ACTIVITY", WISP.alpha_activity),
        ("MYNA_WISP_LINE_WIDTH", WISP.line_width_fraction),
    ];
    defines
        .iter()
        .map(|(name, value)| format!("#define {name} {}", f(*value)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Unroll a stop table into a piecewise `mix()` chain. `tone_of` names the
/// GLSL expression carrying each stop's colour. Panics on a non-increasing
/// table — the table is authored strictly increasing, so this guards a bad
/// retune.
fn emit_gradient(
    fn_name: &str,
    stops: &[GradientStop],
    tone_of: impl Fn(GradientTone) -> String,
) -> String {
    let mut lines = vec![format!(
        "vec4 {fn_name}(float t, vec3 shadowTone, vec3 mainTone, vec3 highlightTone) {{"
    )];
    lines.push(format!("    vec3 rgb = {};", tone_of(stops[0].tone)));
    lines.push(format!("    float a = {};", f(stops[0].alpha)));
    for pair in stops.windows(2) {
        let from = &pair[0];
        let to = &pair[1];
        let span = to.pos - from.pos;
        if span <= 0.0 {
            panic!("ribbon shader: {fn_name} stops must strictly increase");
        }
        lines.push(format!(
            "    if (t >= {} && t <= {}) {{",
            f(from.pos),
            f(to.pos)
        ));
        lines.push(format!(
            "        float u = (t - {}) / {};",
            f(from.pos),
            f(span)
        ));
        lines.push(format!(
            "        rgb = mix({}, {}, u);",
            tone_of(from.tone),
            tone_of(to.tone)
        ));
        lines.push(format!(
            "        a = mix({}, {}, u);",
            f(from.alpha),
            f(to.alpha)
        ));
        lines.push("    }".into());
    }
    lines.push("    return vec4(rgb, a);".into());
    lines.push("}".into());
    lines.join("\n")
}

/// The glow/feather stacks become summed Gaussians — which is what a stack
/// of progressively wider, fainter round strokes was approximating all
/// along, only without the discrete banding.
fn emit_gaussian_stack(fn_name: &str, passes: &[GaussianPass]) -> String {
    let mut lines = vec![
        format!("float {fn_name}(float d, float strokeWidth) {{"),
        "    float total = 0.0;".into(),
        "    float sigma;".into(),
    ];
    for pass in passes {
        lines.push(format!(
            "    sigma = max(strokeWidth * {} * 0.5, 0.5);",
            f(pass.scale)
        ));
        lines.push(format!(
            "    total += {} * exp(-(d * d) / (2.0 * sigma * sigma));",
            f(pass.alpha_scale)
        ));
    }
    lines.push("    return total;".into());
    lines.push("}".into());
    lines.join("\n")
}

/// The wisp tendrils, unrolled from [`WISP_TENDRILS`] (a loop would need the
/// per-tendril tone mix as an array too, for no gain at two entries).
fn emit_wisps() -> String {
    let mut lines = Vec::new();
    for tendril in &WISP_TENDRILS {
        let tone = if tendril.from_shadow {
            format!("mix(uShadow, uMain, {})", f(tendril.mix))
        } else {
            format!("mix(uMain, uHighlight, {})", f(tendril.mix))
        };
        lines.push(format!(
            "    acc = over(acc, wispLayer(t, py, centreY, verticalScale, wispThickness, {}, uElapsedMs + {}, {}, {} * uEffectStrength));",
            f(tendril.seed),
            f(tendril.time_offset_ms),
            tone,
            f(tendril.alpha)
        ));
    }
    lines.join("\n")
}

/// The generated shader: the declarations block plus the fragment body.
#[derive(Clone, Debug, PartialEq)]
pub struct ShaderSource {
    pub declarations: String,
    pub code: String,
}

/// Build the fragment shader (Cogl-style core; wrap with
/// [`standalone_shader`] for a specific GL profile).
///
/// Port of `ribbonGlsl.js`'s `buildRibbonShader`.
pub fn build_ribbon_shader() -> ShaderSource {
    let strand_uniforms = (0..MAX_STRANDS)
        .map(|i| format!("uniform vec4 uStrandGeom{i};\nuniform vec3 uStrandStyle{i};"))
        .collect::<Vec<_>>()
        .join("\n");

    let declarations = format!(
        r#"{defines}

uniform vec2 uSize;
uniform float uElapsedMs;
uniform float uActivity;
uniform float uEffectStrength;
uniform float uBrightnessBoost;
{strand_uniforms}
uniform vec4 uVoice;
uniform vec3 uMain;
uniform vec3 uHighlight;
uniform vec3 uShadow;
uniform vec3 uDotX;
uniform float uDotAlpha;
uniform vec3 uConvergence;

float clamp01(float x) {{
    return clamp(x, 0.0, 1.0);
}}

vec3 lightenRgb(vec3 c, float amount) {{
    return c + (1.0 - c) * clamp01(amount);
}}

// Premultiplied source-over. The renderer blends premultiplied, and
// accumulating in the same space keeps this a single add/lerp per layer.
vec4 over(vec4 dst, vec4 src) {{
    return src + dst * (1.0 - src.a);
}}

vec4 premul(vec3 rgb, float a) {{
    float c = clamp01(a);
    return vec4(rgb * c, c);
}}

// Mirrors the edge taper: a raised cosine, so there is no visible kink
// where the taper begins.
float edgeTaper(float t) {{
    float v = 1.0;
    if (t < MYNA_TAPER_IN)
        v = min(v, (1.0 - cos((t / MYNA_TAPER_IN) * MYNA_PI)) / 2.0);
    if (t > 1.0 - MYNA_TAPER_OUT)
        v = min(v, (1.0 - cos(((1.0 - t) / MYNA_TAPER_OUT) * MYNA_PI)) / 2.0);
    return clamp01(v);
}}

float driftWave(float t, float ms, float freq, float speed, float phase) {{
    return sin(t * freq * MYNA_PI * 2.0 + ms * speed + phase);
}}

float bodyThickness(float t, float ms, float baseThickness, float activity) {{
    float billowAmount = MYNA_BILLOW_MIN + MYNA_BILLOW_ACTIVITY * activity;
    float billow = 1.0 + billowAmount *
        driftWave(t, ms, MYNA_BILLOW_FREQ, MYNA_BILLOW_SPEED, MYNA_BILLOW_PHASE);
    float activityScale = 0.5 + 0.5 * activity;
    float taper = MYNA_TAPER_FLOOR + (1.0 - MYNA_TAPER_FLOOR) * edgeTaper(t);
    return baseThickness * activityScale * taper * billow;
}}

// The strand centreline, regenerated analytically from the same parameters
// that produced the model's sampled points (ribbon's generate_wave_points).
float strandY(float t, float amplitude, float phaseOffset, float delayMs, float speedScale) {{
    float angle = t * MYNA_SPATIAL_FREQUENCY * MYNA_PI * 2.0 +
        phaseOffset + (uElapsedMs - delayMs) * MYNA_FLOW_SPEED * speedScale;
    return sin(angle) * amplitude;
}}

{ribbon_gradient}

{wisp_gradient}

{glow_stack}

{feather_stack}

float roleThickness(float role) {{
    if (role < 0.5)
        return MYNA_THICKNESS_VOICE;
    if (role < 1.5)
        return MYNA_THICKNESS_SECONDARY;
    return MYNA_THICKNESS_BASE;
}}

float roleAlphaScale(float role) {{
    if (role < 0.5)
        return MYNA_ALPHA_VOICE;
    if (role < 1.5)
        return MYNA_ALPHA_SECONDARY;
    return MYNA_ALPHA_BASE;
}}

// A soft-edged disc, antialiased over one pixel.
vec4 disc(vec2 p, vec2 centre, float radius, vec3 rgb, float alpha) {{
    float d = length(p - centre);
    float cov = 1.0 - smoothstep(radius - 1.0, radius + 1.0, d);
    return premul(rgb, alpha * cov);
}}

// One strand, composited over the accumulator. Strands arrive already sorted
// back-to-front by the uploader (PAINT_ORDER: base -> secondary -> voice), so
// this is simply called in index order rather than running a role-matching
// pass per layer.
//
//   geom  = (amplitude, phaseOffset, delayMs, speedScale)
//   style = (alpha, roleTag, active)
vec4 drawStrand(vec4 geom, vec3 style, float t, float py,
                float centreY, float verticalScale, vec4 acc) {{
    float role = style.y;
    if (style.z < 0.5)
        return acc;
    // Below a small activity threshold the depth layers are skipped rather
    // than faded - several near-flat strands stacked at nearly the same
    // position read as stripes, not depth.
    if (role >= 0.5 && uEffectStrength <= 0.0)
        return acc;

    float centre = centreY - strandY(t, geom.x, geom.y, geom.z, geom.w) * verticalScale;
    float d = abs(py - centre);
    float thickness = uSize.y * roleThickness(role) * MYNA_SAFE_SCALE;
    float halfBody = bodyThickness(t, uElapsedMs, thickness, uActivity) * 0.5;

    // The secondary strand is drawn in the shadow tone; the gradient's
    // "main" stops therefore carry that tone.
    vec3 baseRgb = (role > 0.5 && role < 1.5) ? uShadow : uMain;
    baseRgb = lightenRgb(baseRgb, uBrightnessBoost * 0.6);

    float depthActivity = (role < 0.5) ? 1.0 : uEffectStrength;
    float strandAlpha = style.x * roleAlphaScale(role) * depthActivity;

    vec4 grad = ribbonGradient(t, uShadow, baseRgb, uHighlight);

    // Glow sits behind the body, and only under the voice strand.
    if (role < 0.5 && uEffectStrength > 0.0) {{
        float glow = glowStack(d, thickness * 0.5);
        acc = over(acc, premul(grad.rgb, grad.a * strandAlpha * uEffectStrength * glow));
    }}

    // The body edge is feathered by widening its own falloff - which is
    // what the extra edge strokes were emulating.
    float feather = max(1.0, featherStack(0.0, thickness * 0.18) * thickness * uEffectStrength);
    float cov = 1.0 - smoothstep(halfBody - feather, halfBody + feather, d);
    return over(acc, premul(grad.rgb, grad.a * strandAlpha * cov));
}}

vec4 wispLayer(float t, float py, float centreY, float verticalScale,
               float thickness, float seed, float ms, vec3 tone, float baseAlpha) {{
    float centre = centreY - strandY(t, uVoice.x, uVoice.y, uVoice.z, uVoice.w) * verticalScale;
    float curlMagnitude = thickness * (MYNA_WISP_CURL_MIN + MYNA_WISP_CURL_ACTIVITY * uActivity);
    float curl = driftWave(t, ms,
        MYNA_WISP_FREQ_BASE + seed * MYNA_WISP_FREQ_SEED,
        MYNA_WISP_SPEED_BASE + seed * MYNA_WISP_SPEED_SEED,
        seed * MYNA_WISP_PHASE_SEED) *
        curlMagnitude * (MYNA_WISP_TAIL_FLOOR + (1.0 - MYNA_WISP_TAIL_FLOOR) * t);
    float d = abs(py - (centre + curl));
    float sigma = max(thickness * MYNA_WISP_LINE_WIDTH, 0.5);
    float fall = exp(-(d * d) / (2.0 * sigma * sigma));
    float alpha = baseAlpha * (MYNA_WISP_ALPHA_MIN + MYNA_WISP_ALPHA_ACTIVITY * uActivity);
    vec4 grad = wispGradient(t, tone, tone, tone);
    return premul(tone, alpha * grad.a * fall);
}}"#,
        defines = glsl_constant_defines(),
        strand_uniforms = strand_uniforms,
        ribbon_gradient = emit_gradient(
            "ribbonGradient",
            &RIBBON_GRADIENT_STOPS,
            |tone| match tone {
                GradientTone::Shadow => "shadowTone".into(),
                GradientTone::Highlight => "highlightTone".into(),
                GradientTone::Main => "mainTone".into(),
            }
        ),
        wisp_gradient = emit_gradient("wispGradient", &WISP_GRADIENT_STOPS, |_| "mainTone".into()),
        glow_stack = emit_gaussian_stack("glowStack", &GLOW_PASSES),
        feather_stack = emit_gaussian_stack("featherStack", &FEATHER_PASSES),
    );

    let draw_calls = (0..MAX_STRANDS)
        .map(|i| format!("acc = drawStrand(uStrandGeom{i}, uStrandStyle{i}, t, py, centreY, verticalScale, acc);"))
        .collect::<Vec<_>>()
        .join("\n");

    let code = format!(
        r#"vec2 uv = cogl_tex_coord_in[0].xy;
float t = clamp01(uv.x);
vec2 p = vec2(uv.x * uSize.x, uv.y * uSize.y);
float py = p.y;
float centreY = uSize.y * 0.5;
float verticalScale = (uSize.y * 0.5) * MYNA_BASE_CENTRELINE_FRACTION * MYNA_SAFE_SCALE;

vec4 acc = vec4(0.0);

// Strands are uploaded already sorted back-to-front (PAINT_ORDER), so the
// bright focal voice strand lands on top. Unrolled because the per-strand
// parameters are separate uniforms, not an array.
{draw_calls}

// Wispy trailing tendrils curling off the voice strand - soft falloff only,
// no solid body, evoking trailing smoke rather than a second ribbon.
if (uEffectStrength > 0.0) {{
    float wispThickness = uSize.y * MYNA_WISP_THICKNESS_FRACTION * MYNA_SAFE_SCALE;
{wisps}
}}

// morph: three travelling dots crossfading in as the wave fades out.
if (uDotAlpha > 0.0) {{
    vec3 rgb = lightenRgb(uMain, 0.3);
    float radius = uSize.y * 0.09;
    acc = over(acc, disc(p, vec2(uDotX.x * uSize.x, centreY), radius, rgb, uDotAlpha));
    acc = over(acc, disc(p, vec2(uDotX.y * uSize.x, centreY), radius, rgb, uDotAlpha));
    acc = over(acc, disc(p, vec2(uDotX.z * uSize.x, centreY), radius, rgb, uDotAlpha));
}}

// complete: the convergence point, fading on the same curve as its pulse.
if (uConvergence.z > 0.0) {{
    float radius = uSize.y * 0.12 * (1.0 + uBrightnessBoost);
    acc = over(acc, disc(p, vec2(uConvergence.x * uSize.x, centreY - uConvergence.y * verticalScale),
                         radius, lightenRgb(uMain, 0.5), uConvergence.z));
}}

cogl_color_out = acc;"#,
        draw_calls = draw_calls,
        wisps = emit_wisps(),
    );

    ShaderSource { declarations, code }
}

// ── Standalone wrapping (GL profiles) ─────────────────────────────────────

/// The GL profile a [`standalone_shader`] target compiles as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlProfile {
    /// Desktop GLSL 1.20 (the original Cogl-era target).
    Gl120,
    /// GLSL ES 1.00.
    Es100,
    /// GLSL ES 3.00 — GLArea's context profile on Wayland/GLES.
    Es300,
}

/// The vertex attribute carrying the fullscreen quad's corners, in `[0, 1]`.
/// Bound to location 0 so the renderer needs no attribute query.
pub const POSITION_ATTRIBUTE: &str = "aPosition";

/// The varying carrying the quad's `[0, 1]` UV to the fragment stage, where
/// it feeds `cogl_tex_coord_in[0]` — the coordinate Cogl used to supply.
const UV_VARYING: &str = "vUv";

/// The vertex shader for a [`standalone_shader`] fragment: a fullscreen quad
/// whose corners double as the UV Cogl used to hand the snippet.
///
/// The quad is drawn in `[0, 1]` space (mapped to clip space here) so `vUv`
/// is the position unmodified — matching the former Python GPU lab's
/// `ribbon_gl.py`, and therefore the orientation the ribbon was tuned in.
pub fn vertex_shader(profile: GlProfile) -> String {
    let (preamble, attribute, varying_out) = match profile {
        GlProfile::Gl120 => ("#version 120", "attribute", "varying"),
        GlProfile::Es100 => ("#version 100", "attribute", "varying"),
        GlProfile::Es300 => ("#version 300 es", "in", "out"),
    };
    format!(
        "{preamble}\n\
         {attribute} vec2 {POSITION_ATTRIBUTE};\n\
         {varying_out} vec2 {UV_VARYING};\n\
         void main() {{\n\
         {UV_VARYING} = {POSITION_ATTRIBUTE};\n\
         gl_Position = vec4({POSITION_ATTRIBUTE} * 2.0 - 1.0, 0.0, 1.0);\n\
         }}\n"
    )
}

/// Wrap the generated snippet in the surrounding declarations Cogl itself
/// provided, so the fragment is a complete, compilable shader for the target
/// profile. (`cogl_color_in` is declared but unused — the original had it
/// too; glslang does not warn by default.)
///
/// Crucially this also *feeds* `cogl_tex_coord_in[0]` from the vertex
/// stage's UV. Cogl spliced that assignment in itself; without it the
/// snippet still compiles — every strand simply samples x = 0 and the ribbon
/// renders as a degenerate smear — so the wrapper must supply it, and
/// [`vertex_shader`] must be the paired vertex stage.
pub fn standalone_shader(source: &ShaderSource, profile: GlProfile) -> String {
    let (preamble, varying_in, out_assignment) = match profile {
        GlProfile::Gl120 => (
            "#version 120".to_string(),
            format!("varying vec2 {UV_VARYING};"),
            "gl_FragColor = cogl_color_out;".to_string(),
        ),
        GlProfile::Es100 => (
            "#version 100\nprecision highp float;".to_string(),
            format!("varying vec2 {UV_VARYING};"),
            "gl_FragColor = cogl_color_out;".to_string(),
        ),
        GlProfile::Es300 => (
            "#version 300 es\nprecision highp float;\nout vec4 myna_frag_color;".to_string(),
            format!("in vec2 {UV_VARYING};"),
            "myna_frag_color = cogl_color_out;".to_string(),
        ),
    };
    format!(
        "{preamble}\n{varying_in}\nvec4 cogl_color_out;\nvec4 cogl_color_in;\nvec4 cogl_tex_coord_in[4];\n{declarations}\nvoid main() {{\ncogl_tex_coord_in[0] = vec4({UV_VARYING}, 0.0, 1.0);\n{code}\n{out_assignment}\n}}\n",
        declarations = source.declarations,
        code = source.code,
    )
}
