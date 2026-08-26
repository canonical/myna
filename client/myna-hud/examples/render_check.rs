// examples/render_check.rs — the GPU render check (feature 004, T121/T133):
// the port of the former Python lab's `render_headless.py`.
//
// The shader tests prove the generated source *compiles*; this proves a
// driver actually *lights pixels* with it. Two failure modes hide from a
// compile check and show only as an empty overlay:
//
//   1. the program links but every strand falls outside the canvas, and
//   2. `cogl_tex_coord_in[0]` is never fed — Cogl used to splice that
//      assignment in, and without it every strand samples x = 0, so the
//      frame is *constant along x*. This check asserts horizontal variation
//      precisely to catch that.
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
        .application_id("org.myna.HudRenderCheck")
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
                        problems.push(
                            "the frame is constant along x — cogl_tex_coord_in[0] is not being fed"
                                .into(),
                        );
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
