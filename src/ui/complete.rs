//! Folder completion for the library prompt (playback-and-library T7): `Tab`
//! completes the path the way a shell does. The prompt's paths all live under
//! the home folder, so the input is always `~/` followed by a relative path.

use std::path::Path;

/// The prefix every library path starts with; the prompt cannot delete it.
pub const PROMPT_PREFIX: &str = "~/";

/// Completes the last segment of `input` (`~/` and a path under `home`)
/// against the folders it could name. One match is completed with the `/` a
/// shell adds to a folder; several extend the segment to what they share and
/// are returned, sorted, to be shown; none leaves the input as it is. Files
/// are never offered, and hidden folders only once the segment starts with
/// `.`, as in a shell.
#[must_use]
pub fn complete(input: &str, home: &Path) -> (String, Vec<String>) {
    let Some(rest) = input.strip_prefix(PROMPT_PREFIX) else {
        return (input.to_owned(), Vec::new());
    };
    let (parent, partial) = rest.rsplit_once('/').unwrap_or(("", rest));
    let Ok(entries) = std::fs::read_dir(home.join(parent)) else {
        return (input.to_owned(), Vec::new());
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(partial))
        .filter(|name| partial.starts_with('.') || !name.starts_with('.'))
        .collect();
    names.sort();

    let joined = |segment: &str| {
        if parent.is_empty() {
            format!("{PROMPT_PREFIX}{segment}")
        } else {
            format!("{PROMPT_PREFIX}{parent}/{segment}")
        }
    };
    match names.as_slice() {
        [] => (input.to_owned(), Vec::new()),
        [only] => (joined(&format!("{only}/")), Vec::new()),
        [first, others @ ..] => {
            let shared = others.iter().fold(first.as_str(), |shared, name| {
                let length = shared
                    .char_indices()
                    .zip(name.chars())
                    .take_while(|((_, a), b)| a == b)
                    .last()
                    .map_or(0, |((index, a), _)| index + a.len_utf8());
                &shared[..length]
            });
            (joined(shared), names)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::complete;
    use std::fs;

    #[test]
    fn completion_follows_a_shell() {
        let home = std::env::temp_dir().join(format!("velo-complete-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join("Music/Library")).unwrap();
        fs::create_dir_all(home.join("Musings")).unwrap();
        fs::create_dir_all(home.join(".config")).unwrap();
        fs::write(home.join("Mus.txt"), "").unwrap();

        // What two folders share, and both of them offered.
        let (input, offered) = complete("~/Mu", &home);
        assert_eq!(input, "~/Musi");
        assert_eq!(offered, ["Music", "Musings"]);
        // A single folder, completed with its slash, nested too.
        assert_eq!(complete("~/Music", &home).0, "~/Music/");
        assert_eq!(complete("~/Music/L", &home).0, "~/Music/Library/");
        // Nothing matches: unchanged. A hidden folder only for a dot.
        assert_eq!(complete("~/zz", &home), ("~/zz".to_owned(), Vec::new()));
        assert_eq!(complete("~/", &home).1, ["Music", "Musings"]);
        assert_eq!(complete("~/.c", &home).0, "~/.config/");

        let _ = fs::remove_dir_all(&home);
    }
}
