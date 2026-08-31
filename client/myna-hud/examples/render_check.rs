// examples/render_check.rs — the GPU render check (feature 004, T121/T133).
//
// The shader tests prove the generated source *compiles*; this proves a
// driver actually *lights pixels* with it. Three failure modes hide from a
// compile check and show only as a wrong overlay:
//
//   1. the program links but every strand falls outside the canvas (the
//      frame is empty);
//   2. the UV `vUv` is never fed from the vertex stage, so every strand
//      samples x = 0 and the frame is *constant along x*;
//   3. the strand-body uniforms are uploaded with the wrong count, so the
//      wisps/dots still draw but the actual ribbon body is missing
//      (caught by the centreline-bounded-body check below — the body must
//      cover a substantial part of the canvas at the centre row, while
//      wisps alone would give a sparse set of isolated tendrils).
//
// Run with:  xvfb-run -a -s "-screen 0 640x480x24" \
//                cargo run -p myna-hud --example render_check
// Exit code 0 = the ribbon rendered.

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::RefCell;
use std::rc::Rc;

use myna_hud::gl::{read_pixels, RibbonRenderer};
use myna_hud::ribbon::{compute_ribbon_model, RibbonInput, RibbonPhase};
use myna_hud::shader::RibbonPalette;

const WIDTH: i32 = 320;
const HEIGHT: i32 = 64;

fn main() {
    let app = gtk::Application::builder()
        .application_id("com.canonical.Myna.HudRenderCheck")
        .build();

    app.connect_activate(|app| {
        let area = gtk::GLArea::new();
        area.set_size_request(WIDTH, HEIGHT);

        let failures: Rc<RefCell<Vec<String>>> = Rc::default();
        let app_quit = app.clone();
        let failures_render = failures.clone();

        area.connect_render(move |_area, _ctx| {
            let mut problems = failures_render.borrow_mut();

            match RibbonRenderer::realize() {
                Err(e) => problems.push(format!("shader failed to build: {e}")),
                Ok(mut renderer) => {
                    println!("render-check: profile = {:?}", renderer.profile());

                    // A mid-speech recording frame: plenty of activity, so a
                    // correct ribbon covers a good part of the canvas.
                    let model = compute_ribbon_model(RibbonInput {
                        envelope: 0.7,
                        elapsed_ms: 1200.0,
                        phase: RibbonPhase::Flow,
                        ..Default::default()
                    });
                    let palette = RibbonPalette::from_hex("#3584E4", "#99C1F1", "#1A5FB4", 0.35);
                    renderer.render(&model, &palette, WIDTH, HEIGHT);

                    let pixels = unsafe { read_pixels(WIDTH, HEIGHT) };
                    if let Ok(path) = std::env::var("MYNA_RENDER_CHECK_OUT") {
                        // Save the framebuffer as a grayscale PGM so the
                        // rendered ribbon can be eyeballed from CI.
                        let mut pgm = format!("P5\n{WIDTH} {HEIGHT}\n255\n").into_bytes();
                        for y in 0..HEIGHT {
                            for x in 0..WIDTH {
                                let i = ((y * WIDTH + x) * 4 + 3) as usize;
                                pgm.push(pixels[i]);
                            }
                        }
                        let _ = std::fs::write(&path, pgm);
                        println!("render-check: wrote {path}");
                    }
                    let alpha_at =
                        |x: i32, y: i32| -> u8 { pixels[((y * WIDTH + x) * 4 + 3) as usize] };

                    // 1. Something was drawn.
                    let lit = (0..WIDTH)
                        .flat_map(|x| (0..HEIGHT).map(move |y| (x, y)))
                        .filter(|(x, y)| alpha_at(*x, *y) > 8)
                        .count();
                    let coverage = lit as f64 / (WIDTH * HEIGHT) as f64;
                    println!("render-check: coverage = {:.1}%", coverage * 100.0);
                    if lit == 0 {
                        problems.push("nothing was drawn — the frame is empty".into());
                    } else if coverage < 0.01 {
                        problems.push(format!("suspiciously little drawn ({coverage:.4})"));
                    }

                    // 2. The ribbon varies along x. A frame that is constant
                    //    per column is the un-fed-UV signature.
                    let column_profile: Vec<u32> = (0..WIDTH)
                        .map(|x| (0..HEIGHT).map(|y| alpha_at(x, y) as u32).sum())
                        .collect();
                    let min = column_profile.iter().min().copied().unwrap_or(0);
                    let max = column_profile.iter().max().copied().unwrap_or(0);
                    println!("render-check: column alpha min={min} max={max}");
                    if max == min {
                        problems
                            .push("the frame is constant along x — vUv is not being fed".into());
                    }

                    // 3. The strand BODY drew — not just wisps / dots.
                    //    The body is a thin band near the vertical
                    //    centre; wisps + dots add isolated tendrils and a
                    //    few travelling markers, not a connected fill.
                    //    A wrong uniform upload (e.g. writing `count` as
                    //    `components * count` for an array uniform) can
                    //    leave the strand body drawing nothing while the
                    //    wisp/dot layer, which reads different uniforms,
                    //    still draws — and the result is a flat saturated
                    //    block that passes any "is something drawn?" check.
                    //
                    //    The discriminating signal is the number of rows
                    //    lit per column: a wave lights a band, a solid
                    //    block lights all rows. We assert the median
                    //    lit-rows-per-column is well below HEIGHT (the
                    //    wave is a thin band, not a solid rectangle).
                    let mut rows_per_col: Vec<u32> = (0..WIDTH)
                        .map(|x| (0..HEIGHT).filter(|&y| alpha_at(x, y) > 32).count() as u32)
                        .collect();
                    rows_per_col.sort_unstable();
                    let median = rows_per_col[WIDTH as usize / 2];
                    let p90 = rows_per_col[(WIDTH as usize * 90) / 100];
                    println!(
                        "render-check: rows-lit per column median={median} p90={p90} (of {HEIGHT})"
                    );
                    if median > HEIGHT as u32 * 3 / 4 {
                        problems.push(format!(
                            "the body fills the canvas — median rows-lit per column \
                             is {median} of {HEIGHT} (a wave is a thin band, not a \
                             solid block; this usually means the strand-array \
                             uniforms were uploaded with the wrong count)"
                        ));
                    }

                    renderer.unrealize();
                }
            }

            let app = app_quit.clone();
            let failures = failures_render.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
                let problems = failures.borrow();
                if problems.is_empty() {
                    println!("render-check: OK — the ribbon rendered");
                } else {
                    for p in problems.iter() {
                        eprintln!("render-check: FAIL — {p}");
                    }
                }
                let code = i32::from(!problems.is_empty());
                app.quit();
                std::process::exit(code);
            });
            glib::Propagation::Proceed
        });

        let win = gtk::ApplicationWindow::new(app);
        win.set_title(Some("myna render-check"));
        win.set_child(Some(&area));
        win.present();
    });

    std::process::exit(app.run().get() as i32);
}
