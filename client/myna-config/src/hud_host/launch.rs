//! How to start the HUD: `$MYNA_HUD_BINARY` if set (a build tree's binary,
//! which a packaged snap must not shadow), else the snap's `myna.hud` app.

pub const OVERRIDE_ENV: &str = "MYNA_HUD_BINARY";

/// What to run to host the HUD on X11, or None when nothing can be.
pub fn argv(
    getenv: impl Fn(&str) -> Option<String>,
    is_executable: impl Fn(&str) -> bool,
) -> Result<Vec<String>, String> {
    let host = ["--host", "x11"].map(str::to_owned);
    let mut argv = match getenv(OVERRIDE_ENV).filter(|path| !path.is_empty()) {
        // A bad override is a configuration error, never a silent fallback.
        Some(path) if is_executable(&path) => vec![path],
        Some(path) => return Err(format!("{OVERRIDE_ENV} is not executable: {path}")),
        None if is_executable("snap") || is_executable("/usr/bin/snap") => {
            ["snap", "run", "myna.hud"].map(str::to_owned).to_vec()
        }
        None => return Err("snap is not installed".to_owned()),
    };
    argv.extend(host);
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn the_packaged_hud_is_the_snap_app() {
        let got = argv(env(&[]), |path| path == "/usr/bin/snap").unwrap();
        assert_eq!(got, ["snap", "run", "myna.hud", "--host", "x11"]);
    }

    #[test]
    fn the_override_wins_over_the_snap() {
        let got = argv(env(&[(OVERRIDE_ENV, "/t/myna-hud")]), |_| true).unwrap();
        assert_eq!(got, ["/t/myna-hud", "--host", "x11"]);
    }

    #[test]
    fn a_missing_override_is_an_error_not_a_fallback() {
        let error = argv(env(&[(OVERRIDE_ENV, "/t/none")]), |path| path != "/t/none").unwrap_err();
        assert!(error.contains("/t/none"));
    }

    #[test]
    fn an_empty_override_is_unset() {
        let got = argv(env(&[(OVERRIDE_ENV, "")]), |path| path == "snap").unwrap();
        assert_eq!(got[0], "snap");
    }

    #[test]
    fn no_snap_and_no_override_is_an_error() {
        assert!(argv(env(&[]), |_| false).is_err());
    }
}
