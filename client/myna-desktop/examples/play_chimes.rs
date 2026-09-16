//! Manual listening check for the dictation chimes — plays start, stop, and
//! error once each through the real PipeWire path (no daemon, no hotkey, no
//! IBus). Run with:
//!
//! ```sh
//! cargo run -p myna-desktop --example play_chimes
//! ```

use myna_desktop::{Chime, ChimePlayer, PipeWireChimePlayer};
use std::thread::sleep;
use std::time::Duration;

fn main() {
    let mut player = PipeWireChimePlayer;
    for (name, chime) in [
        ("start", Chime::Start),
        ("stop", Chime::Stop),
        ("error", Chime::Error),
    ] {
        println!("playing {name}...");
        player.play(chime);
        // `play` is fire-and-forget (spawns its own thread); give each clip
        // time to actually finish before the next one starts.
        sleep(Duration::from_millis(800));
    }
}
