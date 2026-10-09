//! What an application is called, from the desktop entries xfce4-keyboard-
//! settings shows: Xfce stores a shortcut's command, not a label.

use std::path::{Path, PathBuf};

use gio::glib;

/// The directories of desktop entries, the user's first.
pub fn application_dirs() -> Vec<PathBuf> {
    std::iter::once(glib::user_data_dir())
        .chain(glib::system_data_dirs())
        .map(|dir| dir.join("applications"))
        .collect()
}

/// The Name of the entry in `dirs` that launches `command`: the one whose
/// `Exec` is it, else, for a bare program, the first that starts that
/// program. None when no entry does.
pub fn name_of(command: &str, dirs: &[PathBuf]) -> Option<String> {
    let command = words(command)?;
    let mut by_program = None;
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            let Some((exec, name)) = read(&path) else {
                continue;
            };
            let Some(exec) = words(&exec) else { continue };
            if exec == command {
                return Some(name);
            }
            if command.len() == 1 && exec.first() == command.first() && by_program.is_none() {
                by_program = Some(name);
            }
        }
    }
    by_program
}

/// `command` as program and arguments, the program by its file name and the
/// field codes (`%U`) of an `Exec` line dropped.
fn words(command: &str) -> Option<Vec<String>> {
    let mut words = glib::shell_parse_argv(command).ok()?.into_iter();
    let program = words.next()?;
    let program = Path::new(&program).file_name()?.to_str()?.to_owned();
    Some(
        std::iter::once(program)
            .chain(
                words
                    .filter_map(|word| word.into_string().ok())
                    .filter(|word| !(word.len() == 2 && word.starts_with('%'))),
            )
            .collect(),
    )
}

/// An application entry's `Exec` and Name, as shown in the user's language.
fn read(path: &Path) -> Option<(String, String)> {
    if path.extension()? != "desktop" {
        return None;
    }
    let file = glib::KeyFile::new();
    file.load_from_file(path, glib::KeyFileFlags::NONE).ok()?;
    let group = "Desktop Entry";
    if file.string(group, "Type").ok()? != "Application" {
        return None;
    }
    let exec = file.string(group, "Exec").ok()?.to_string();
    let name = file.locale_string(group, "Name", None).ok()?.to_string();
    Some((exec, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(entries: &[(&str, &str, &str)]) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "myna-apps-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (file, exec, name) in entries {
            std::fs::write(
                dir.join(file),
                format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\n"),
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn a_program_is_named_by_the_entry_that_starts_it() {
        let dir = dir_with(&[
            ("thunar.desktop", "thunar %U", "File Manager"),
            (
                "thunar-settings.desktop",
                "thunar-settings",
                "File Manager Settings",
            ),
        ]);
        let dirs = [dir.clone()];
        assert_eq!(name_of("thunar", &dirs).as_deref(), Some("File Manager"));
        assert_eq!(
            name_of("/usr/bin/thunar", &dirs).as_deref(),
            Some("File Manager")
        );
        assert_eq!(name_of("firefox", &dirs), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn arguments_must_match_unless_the_command_is_a_bare_program() {
        let dir = dir_with(&[
            (
                "terminal.desktop",
                "exo-open --launch TerminalEmulator",
                "Terminal Emulator",
            ),
            (
                "mail.desktop",
                "exo-open --launch MailReader",
                "Mail Reader",
            ),
        ]);
        let dirs = [dir.clone()];
        assert_eq!(
            name_of("exo-open --launch MailReader", &dirs).as_deref(),
            Some("Mail Reader")
        );
        assert_eq!(name_of("exo-open --launch WebBrowser", &dirs), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_first_directory_wins_and_non_applications_are_skipped() {
        let user = dir_with(&[("a.desktop", "tool", "Mine")]);
        let system = dir_with(&[("a.desktop", "tool", "Theirs")]);
        std::fs::write(
            system.join("link.desktop"),
            "[Desktop Entry]\nType=Link\nName=Not an app\nExec=tool\n",
        )
        .unwrap();
        assert_eq!(
            name_of("tool", &[user.clone(), system.clone()]).as_deref(),
            Some("Mine")
        );
        assert_eq!(
            name_of("tool", std::slice::from_ref(&system)).as_deref(),
            Some("Theirs")
        );
        for dir in [user, system] {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}
