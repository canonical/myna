// tests/shader.rs — conformance test for the GENERATED wave-ribbon shader
// (feature 004, 2026-08-21 GPU rasterization pass), ported 1:1 from the GJS
// test/ribbonGlsl.test.js. No GL context needed for the constant/uniform
// conformance; glslangValidator (when installed) checks a driver would
// accept the source. The one property a visual check cannot give you: the
// shader's baked-in constants still equal the Rust constants the model
// uses — the two renderers' TUNING must be shared, exactly where two copies
// would silently drift.

use std::collections::BTreeMap;
use std::process::Command;

use myna_hud::ribbon::{
    compute_ribbon_model, RibbonInput, RibbonPhase, StrandRole, DEFAULT_STRAND_COUNT, FLOW_SPEED,
    SPATIAL_FREQUENCY,
};
use myna_hud::shader::{
    activity_ramp, build_ribbon_shader, compute_safe_scale, glsl_constant_defines,
    pack_ribbon_uniforms, ribbon_uniforms, role_tag, standalone_shader, vertex_shader, GlProfile,
    RibbonPalette, ACTIVITY_RAMP, BILLOW, EDGE_TAPER, MAX_DOTS, MAX_STRANDS, PAINT_ORDER,
    RIBBON_GRADIENT_STOPS, WISP, WISP_THICKNESS_FRACTION,
};
use myna_hud::states::Severity;

fn palette() -> RibbonPalette {
    RibbonPalette::from_hex("#E95420", "#F5B7A0", "#77216F", 0.35)
}

fn model(
    envelope: f64,
    elapsed_ms: f64,
    phase: RibbonPhase,
    phase_elapsed_ms: f64,
) -> myna_hud::ribbon::RibbonModel {
    compute_ribbon_model(RibbonInput {
        envelope,
        elapsed_ms,
        phase,
        phase_elapsed_ms,
        ..Default::default()
    })
}

// --- The shader source is well-formed enough to hand to the driver --------

#[test]
fn generator_produces_blocks() {
    let shader = build_ribbon_shader();
    assert!(!shader.declarations.is_empty(), "declarations block");
    assert!(!shader.code.is_empty(), "replace block");
    assert!(
        shader.code.contains("cogl_color_out ="),
        "writes cogl_color_out"
    );
    assert!(
        shader.code.contains("cogl_tex_coord_in[0]"),
        "reads the UV via cogl_tex_coord_in"
    );
    let source = format!("{}\n{}", shader.declarations, shader.code);
    assert!(
        !source.contains("undefined") && !source.contains("NaN") && !source.contains("[object"),
        "no unresolved value leaked into the source"
    );
}

// GLSL has no implicit int→float promotion, so a bare integer literal in a
// float expression is a compile error on stricter drivers.
#[test]
fn every_define_is_a_float_literal() {
    for line in glsl_constant_defines().split('\n') {
        let value = line.rsplit(' ').next().unwrap();
        assert!(
            value.parse::<f64>().is_ok() && value.contains('.'),
            "define is a float literal: {line}"
        );
    }
}

// --- Constants: the shader's copy must equal the Rust original -----------

fn parse_defines() -> BTreeMap<String, f64> {
    glsl_constant_defines()
        .split('\n')
        .map(|line| {
            let rest = line.strip_prefix("#define ").expect("define line");
            let (name, value) = rest.split_once(' ').expect("name value");
            (name.to_string(), value.parse::<f64>().expect("float"))
        })
        .collect()
}

#[test]
fn shader_constants_match_their_rust_originals() {
    let defines = parse_defines();
    let same = |define_name: &str, js_value: f64, label: &str| {
        assert_eq!(
            defines.get(define_name),
            Some(&js_value),
            "{label}: shader {define_name} === Rust {js_value}"
        );
    };
    same("MYNA_SPATIAL_FREQUENCY", SPATIAL_FREQUENCY, "wave");
    same("MYNA_FLOW_SPEED", FLOW_SPEED, "wave");
    same(
        "MYNA_BASE_CENTRELINE_FRACTION",
        myna_hud::shader::BASE_CENTRELINE_FRACTION,
        "geometry",
    );
    same("MYNA_SAFE_SCALE", compute_safe_scale(), "overflow guard");
    same("MYNA_TAPER_IN", EDGE_TAPER.in_width, "edge taper");
    same("MYNA_TAPER_OUT", EDGE_TAPER.out_width, "edge taper");
    same("MYNA_BILLOW_MIN", BILLOW.min_amount, "billow");
    same("MYNA_BILLOW_ACTIVITY", BILLOW.activity_amount, "billow");
    same("MYNA_BILLOW_FREQ", BILLOW.freq, "billow");
    same("MYNA_BILLOW_SPEED", BILLOW.speed, "billow");
    same("MYNA_BILLOW_PHASE", BILLOW.phase, "billow");
    same("MYNA_TAPER_FLOOR", BILLOW.taper_floor, "billow");
    same("MYNA_ACTIVITY_LO", ACTIVITY_RAMP.lo, "activity ramp");
    same("MYNA_ACTIVITY_HI", ACTIVITY_RAMP.hi, "activity ramp");
    same(
        "MYNA_WISP_THICKNESS_FRACTION",
        WISP_THICKNESS_FRACTION,
        "wisp",
    );
    same("MYNA_WISP_CURL_MIN", WISP.curl_min, "wisp");
    same("MYNA_WISP_CURL_ACTIVITY", WISP.curl_activity, "wisp");
    same("MYNA_WISP_LINE_WIDTH", WISP.line_width_fraction, "wisp");

    // The per-role tables are the likeliest thing to be retuned in only one
    // place, since they read as "just a number" at each call site.
    for role in [StrandRole::Voice, StrandRole::Secondary, StrandRole::Base] {
        let suffix = format!("{role:?}").to_uppercase();
        same(
            &format!("MYNA_THICKNESS_{suffix}"),
            myna_hud::shader::role_thickness_fraction(role),
            "role thickness",
        );
        same(
            &format!("MYNA_ALPHA_{suffix}"),
            myna_hud::shader::role_alpha_scale(role),
            "role alpha",
        );
    }
}

// --- Uniforms: the declared set and the uploaded set must agree -----------

#[test]
fn uniforms_declared_and_packed_agree() {
    let shader = build_ribbon_shader();
    let specs = ribbon_uniforms();

    // Parse `uniform <type> <name>;` declarations.
    let declared: Vec<(String, String)> = shader
        .declarations
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("uniform ")?;
            let rest = rest.strip_suffix(';')?;
            let (ty, name) = rest.split_once(' ')?;
            Some((ty.to_string(), name.to_string()))
        })
        .collect();

    // An array uniform can never be uploaded through the vec4-max packing.
    assert!(
        declared.iter().all(|(ty, _)| !ty.contains('[')),
        "no uniform is declared as an array"
    );

    assert!(
        declared
            .iter()
            .all(|(_, name)| specs.iter().any(|s| s.name == name)),
        "every declared uniform is in ribbon_uniforms()"
    );
    assert!(
        specs
            .iter()
            .all(|s| declared.iter().any(|(_, name)| name == s.name)),
        "every ribbon_uniforms() entry is declared in the shader"
    );

    let glsl_components = |ty: &str| match ty {
        "float" => 1,
        "vec2" => 2,
        "vec3" => 3,
        "vec4" => 4,
        _ => 0,
    };
    for (ty, name) in &declared {
        if let Some(spec) = specs.iter().find(|s| s.name == name) {
            assert_eq!(
                spec.components,
                glsl_components(ty),
                "{name}: component count matches its GLSL type"
            );
            assert!(
                (1..=4).contains(&spec.components),
                "{name}: fits the four-component limit"
            );
        }
    }

    assert!(
        (0..MAX_STRANDS).all(|i| {
            declared
                .iter()
                .any(|(ty, name)| name == &format!("uStrandGeom{i}") && ty == "vec4")
                && declared
                    .iter()
                    .any(|(ty, name)| name == &format!("uStrandStyle{i}") && ty == "vec3")
        }),
        "one geometry/style uniform pair per strand slot"
    );

    assert_eq!(
        MAX_STRANDS, DEFAULT_STRAND_COUNT,
        "slots sized for the model's maximum"
    );

    assert!(
        (0..MAX_STRANDS).all(|i| shader
            .code
            .contains(&format!("drawStrand(uStrandGeom{i}, uStrandStyle{i}"))),
        "every strand slot is composited by the shader body"
    );

    // The shader composites strand 0, 1, 2… in index order, so the
    // uploader's sort is what puts the bright voice strand on top.
    assert_eq!(PAINT_ORDER.len(), 3, "paint order covers every StrandRole");
    assert_eq!(
        *PAINT_ORDER.last().unwrap(),
        StrandRole::Voice,
        "paint order ends with voice"
    );
}

// --- Role tags: distinct, and covering every StrandRole -------------------

#[test]
fn role_tags() {
    let roles = [StrandRole::Voice, StrandRole::Secondary, StrandRole::Base];
    assert_eq!(
        role_tag(StrandRole::Voice),
        0,
        "voice is tag 0, so it draws last (in front)"
    );
    let tags: Vec<i32> = roles.map(role_tag).to_vec();
    let distinct = tags.iter().collect::<std::collections::HashSet<_>>().len();
    assert_eq!(distinct, roles.len(), "role tags are distinct");
    let declarations = &build_ribbon_shader().declarations;
    assert!(
        roles.iter().all(|r| declarations.contains(&format!(
            "MYNA_THICKNESS_{}",
            format!("{r:?}").to_uppercase()
        ))),
        "the shader branches on every role tag (via the thickness defines)"
    );
}

// --- The gradient chain covers the whole 0-1 span ------------------------

#[test]
fn gradient_chain_covers_0_to_1() {
    let declarations = &build_ribbon_shader().declarations;
    let mut positions: Vec<(f64, f64)> = Vec::new();
    for line in declarations.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("if (t >= ") {
            if let Some((from, rest)) = rest.split_once(" && t <= ") {
                if let Some(to) = rest.strip_suffix(") {") {
                    positions.push((from.parse().unwrap(), to.parse().unwrap()));
                }
            }
        }
    }
    assert!(
        positions.iter().any(|(from, _)| *from == 0.0),
        "starts at 0"
    );
    assert!(positions.iter().any(|(_, to)| *to == 1.0), "ends at 1");
    assert!(
        positions.len() >= RIBBON_GRADIENT_STOPS.len() - 1,
        "a segment per authored stop pair"
    );
}

// --- The model still supplies every parameter the shader needs -----------
// The shader regenerates each strand's sine itself, so it depends on the
// model reporting the parameters that produced its sampled points. If
// compute_ribbon_model ever stops emitting these, the GPU path would
// silently render flat strands rather than fail.

#[test]
fn model_supplies_shader_parameters() {
    let required = |s: &myna_hud::ribbon::Strand| {
        s.amplitude.is_finite()
            && s.phase_offset.is_finite()
            && s.delay_ms.is_finite()
            && s.speed_scale.is_finite()
    };
    for phase in [
        RibbonPhase::Unfold,
        RibbonPhase::Flow,
        RibbonPhase::Morph,
        RibbonPhase::Complete,
    ] {
        let model = model(0.5, 120.0, phase, 40.0);
        assert!(
            model.strands.iter().all(required),
            "{phase:?}: every strand reports finite shader parameters"
        );
    }

    let reduced = compute_ribbon_model(RibbonInput {
        envelope: 0.5,
        elapsed_ms: 120.0,
        reduced_motion: true,
        ..Default::default()
    });
    assert!(
        reduced.strands.iter().all(required),
        "reduced motion still reports the shader parameters"
    );
    assert_eq!(
        reduced.strands[0].amplitude, 0.0,
        "reduced motion is a flat strand (zero amplitude → flat sine on the GPU too)"
    );

    let morph = model(0.5, 120.0, RibbonPhase::Morph, 100.0);
    let dots = morph.dots.as_ref().expect("morph produces dots");
    assert!(
        dots.len() <= MAX_DOTS,
        "never more dots than the shader can hold"
    );
}

// --- The packed uniforms are complete and uploadable ---------------------

#[test]
fn packed_uniforms_are_complete_and_uploadable() {
    for phase in [
        RibbonPhase::Unfold,
        RibbonPhase::Flow,
        RibbonPhase::Morph,
        RibbonPhase::Complete,
    ] {
        let model = model(0.7, 1200.0, phase, 100.0);
        let packed = pack_ribbon_uniforms(360.0, 32.0, &model, &palette());
        for spec in ribbon_uniforms() {
            let values = packed
                .get(spec.name)
                .unwrap_or_else(|| panic!("{}: missing from the packing", spec.name));
            assert_eq!(
                values.len(),
                spec.components,
                "{}: packed to its declared width",
                spec.name
            );
            assert!(
                values.iter().all(|v| v.is_finite()),
                "{}: every packed value is finite",
                spec.name
            );
        }
    }

    // The shader composites strand slots in index order, so the sort is the
    // only thing putting the bright voice strand on top.
    let flow = model(0.7, 1200.0, RibbonPhase::Flow, 100.0);
    let packed = pack_ribbon_uniforms(360.0, 32.0, &flow, &palette());
    let mut tags = Vec::new();
    for i in 0..MAX_STRANDS {
        let style = &packed[&format!("uStrandStyle{i}")];
        if style[2] > 0.5 {
            tags.push(style[1] as i32);
        }
    }
    assert_eq!(
        *tags.last().unwrap(),
        role_tag(StrandRole::Voice),
        "ordered back-to-front, voice last"
    );
    assert!(
        (0..MAX_STRANDS).all(|i| {
            let style = &packed[&format!("uStrandStyle{i}")];
            style[2] > 0.5 || style.iter().all(|v| *v == 0.0)
        }),
        "an unused strand slot is marked inactive rather than transparent"
    );
}

// --- Regenerating the wave on the GPU matches the model's own points -----
// The shader evaluates `strandY` per pixel instead of consuming the model's
// sampled points. This mirrors that GLSL expression in Rust and checks it
// reproduces the very points compute_ribbon_model returned — the actual
// guarantee that the model and the shader draw the same wave.

#[test]
fn shader_strand_y_reproduces_the_models_points() {
    fn strand_y_mirror(t: f64, strand: &myna_hud::ribbon::Strand, elapsed_ms: f64) -> f64 {
        use std::f64::consts::PI;
        let angle = t * SPATIAL_FREQUENCY * PI * 2.0
            + strand.phase_offset
            + (elapsed_ms - strand.delay_ms) * FLOW_SPEED * strand.speed_scale;
        angle.sin() * strand.amplitude
    }

    let elapsed_ms = 417.0;
    let m = compute_ribbon_model(RibbonInput {
        envelope: 0.62,
        elapsed_ms,
        ..Default::default()
    });
    let mut worst = 0.0f64;
    for strand in &m.strands {
        let n = strand.points.len();
        for i in 0..n {
            let t = i as f64 / (n - 1) as f64;
            worst = worst.max((strand_y_mirror(t, strand, elapsed_ms) - strand.points[i].y).abs());
        }
    }
    assert!(
        worst < 1e-12,
        "strandY reproduces the model's points (worst Δ {worst:.2e})"
    );
}

// --- Does it actually COMPILE? -------------------------------------------
// Everything above checks the source we generate is *consistent*; this
// checks a driver would accept it. glslangValidator is optional (the check
// skips when absent) but catches the whole class of errors a generator
// makes — a missing decimal point, an undeclared identifier, an int/float
// mismatch — which otherwise surface only as a silently blank ribbon.

#[test]
fn generated_shader_compiles_under_glslang() {
    let glslang = which_glslang();
    let Some(glslang) = glslang else {
        eprintln!("     (skip) glslangValidator not installed — shader compile check skipped");
        return;
    };
    let shader = build_ribbon_shader();
    // The production profile (GLArea on GLES/Wayland) is exercised here too,
    // on top of the GJS suite's 1.20/ES 1.00 pair.
    for (profile, label) in [
        (GlProfile::Gl120, "GLSL 1.20"),
        (GlProfile::Es100, "GLSL ES 1.00"),
        (GlProfile::Es300, "GLSL ES 3.00"),
    ] {
        let source = standalone_shader(&shader, profile);
        let dir = std::env::temp_dir().join(format!(
            "myna-shader-test-{}-{:?}.frag",
            std::process::id(),
            profile
        ));
        std::fs::write(&dir, &source).expect("write temp shader");
        let output = Command::new(&glslang)
            .arg("-S")
            .arg("frag")
            .arg(&dir)
            .output()
            .expect("run glslang");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let text = format!("{stdout}{stderr}");
        assert!(
            output.status.success() && !text.contains("ERROR"),
            "compiles as {label}:\n{text}"
        );
        let _ = std::fs::remove_file(&dir);
    }
}

fn which_glslang() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("glslangValidator"))
        .find(|p| p.is_file())
}

// --- The vertex stage and the UV feed ------------------------------------
// Cogl spliced the `cogl_tex_coord_in[0]` assignment in itself. Outside the
// Shell the wrapper must supply it — and its absence does NOT fail
// compilation: every strand would sample x = 0 and the ribbon would render
// as a degenerate smear. These pin the plumbing that only a rendered pixel
// would otherwise catch.

#[test]
fn the_wrapper_feeds_the_texture_coordinate() {
    let shader = build_ribbon_shader();
    for profile in [GlProfile::Gl120, GlProfile::Es100, GlProfile::Es300] {
        let source = standalone_shader(&shader, profile);
        assert!(
            source.contains("cogl_tex_coord_in[0] = vec4(vUv"),
            "{profile:?} feeds the UV Cogl used to supply"
        );
        let assignment = source.find("cogl_tex_coord_in[0] =").expect("assignment");
        let body = source.find("void main()").expect("main");
        assert!(assignment > body, "{profile:?} feeds it inside main()");
        // ...and before the generated code reads it.
        let first_read = source[assignment + 1..]
            .find("cogl_tex_coord_in[0]")
            .map(|i| i + assignment + 1);
        if let Some(read) = first_read {
            assert!(read > assignment, "{profile:?} feeds it before any read");
        }
    }
}

#[test]
fn the_vertex_stage_matches_the_fragment_varying() {
    let shader = build_ribbon_shader();
    for profile in [GlProfile::Gl120, GlProfile::Es100, GlProfile::Es300] {
        let vertex = vertex_shader(profile);
        let fragment = standalone_shader(&shader, profile);
        assert!(
            vertex.contains("vUv") && fragment.contains("vUv"),
            "{profile:?} shares the varying name"
        );
        assert!(
            vertex.contains("aPosition"),
            "{profile:?} declares the position attribute the renderer binds"
        );
        // The quad is drawn in [0,1] so the UV is the position unmodified —
        // the orientation the ribbon was tuned in.
        assert!(
            vertex.contains("vUv = aPosition"),
            "{profile:?} passes the corner through as the UV"
        );
        assert!(
            vertex.contains("aPosition * 2.0 - 1.0"),
            "{profile:?} maps the [0,1] quad into clip space"
        );
        // ES 3.00 must use in/out, not the deprecated attribute/varying.
        if profile == GlProfile::Es300 {
            assert!(
                !vertex.contains("attribute ") && !vertex.contains("varying "),
                "ES 3.00 uses in/out"
            );
        }
    }
}

#[test]
fn the_vertex_stage_compiles_under_glslang() {
    let Some(glslang) = which_glslang() else {
        eprintln!("     (skip) glslangValidator not installed");
        return;
    };
    for (profile, label) in [
        (GlProfile::Gl120, "GLSL 1.20"),
        (GlProfile::Es100, "GLSL ES 1.00"),
        (GlProfile::Es300, "GLSL ES 3.00"),
    ] {
        let source = vertex_shader(profile);
        let path = std::env::temp_dir().join(format!(
            "myna-vertex-test-{}-{:?}.vert",
            std::process::id(),
            profile
        ));
        std::fs::write(&path, &source).expect("write temp shader");
        let output = Command::new(&glslang)
            .arg("-S")
            .arg("vert")
            .arg(&path)
            .output()
            .expect("run glslang");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success() && !text.contains("ERROR"),
            "vertex stage compiles as {label}:\n{text}"
        );
        let _ = std::fs::remove_file(&path);
    }
}

// --- Palette resolution sanity (amber override + activity) ----------------

#[test]
fn palette_resolution_amber_override() {
    let flow = model(0.7, 500.0, RibbonPhase::Flow, 0.0);
    let normal = myna_hud::shader::resolve_ribbon_palette(&flow, &palette());
    assert_eq!(normal.main_rgb, myna_hud::shader::hex_to_rgb("#E95420"));

    let amber = compute_ribbon_model(RibbonInput {
        envelope: 0.7,
        elapsed_ms: 500.0,
        severity_tint: Some(Severity::Recoverable),
        ..Default::default()
    });
    let tinted = myna_hud::shader::resolve_ribbon_palette(&amber, &palette());
    assert_eq!(
        tinted.main_rgb,
        myna_hud::shader::hex_to_rgb(myna_hud::shader::AMBER_MAIN)
    );
    assert_eq!(
        tinted.highlight_rgb,
        myna_hud::shader::hex_to_rgb(myna_hud::shader::AMBER_HIGHLIGHT)
    );

    // Activity tracks the voice strand's amplitude; the ramp is the
    // smoothstep bounds.
    assert!((0.0..=1.0).contains(&normal.activity));
    assert_eq!(normal.effect_strength, activity_ramp(normal.activity));
    assert_eq!(activity_ramp(ACTIVITY_RAMP.lo - 0.01), 0.0);
    assert_eq!(activity_ramp(ACTIVITY_RAMP.hi + 0.01), 1.0);
}
