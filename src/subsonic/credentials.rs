//! The credential resolver: where the server's secret comes from (T12a).
//!
//! Resolution order, one rule in [`resolve`]: the config's `[subsonic]`
//! `api_key` when present, else `$VELO_SUBSONIC_SECRET`, else the file
//! `$XDG_CONFIG_HOME/velo/subsonic.key` (else `~/.config/velo/subsonic.key`)
//! read as a single line. The environment values and the home directory are
//! arguments, not reads, so a test never touches the user's environment and
//! the repository never needs `set_var`.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use super::Error;

/// How wide the key file's permissions must be before velo warns: only the
/// owner may read or write it.
const PRIVATE_MASK: u32 = 0o077;

/// The resolved credentials for one Subsonic server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    /// The server base URL, normalized: no trailing slash, ready for
    /// `/rest/…` to be appended.
    pub url: String,
    /// The Subsonic user, sent as `u` with every request.
    pub user: String,
    /// The secret. When the server advertises the `apiKeyAuthentication`
    /// extension it is sent as `apiKey`; otherwise it is the password the
    /// token auth derives from (`t = md5(password + salt)`).
    pub secret: String,
    /// One-line warnings to show on stderr, e.g. for a key file that is
    /// readable by group or others. Warnings are advice, not failures.
    pub warnings: Vec<String>,
}

/// Reads the secret out of the key file, warning when the file is readable
/// by group or others. The path is named in every message.
fn read_key_file(path: &Path) -> Result<(String, Vec<String>), Error> {
    let secret = fs::read_to_string(path).map_err(|error| Error::Credentials(format!(
        "cannot read the subsonic key file {}: {error} — set `api_key` in the [subsonic] table, or $VELO_SUBSONIC_SECRET, or write the password to this file as one line",
        path.display()
    )))?;
    let secret = secret.trim();
    if secret.contains('\n') || secret.contains('\r') {
        return Err(Error::Credentials(format!(
            "the subsonic key file {} must be a single line",
            path.display()
        )));
    }
    if secret.is_empty() {
        return Err(Error::Credentials(format!(
            "the subsonic key file {} is empty",
            path.display()
        )));
    }

    let mut warnings = Vec::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let mode = fs::metadata(path)
            .map_err(|error| {
                Error::Credentials(format!(
                    "cannot stat the subsonic key file {}: {error}",
                    path.display()
                ))
            })?
            .mode();
        if mode & PRIVATE_MASK != 0 {
            warnings.push(format!(
                "the subsonic key file {} is readable by group or others (mode {:04o}) — restrict it to you alone: chmod 600",
                path.display(),
                mode & 0o777
            ));
        }
    }
    Ok((secret.to_owned(), warnings))
}

/// Where the secret should live when neither the config nor the environment
/// has one: `$XDG_CONFIG_HOME/velo/subsonic.key`, else
/// `$HOME/.config/velo/subsonic.key`. `None` when neither environment value
/// is set.
#[must_use]
pub fn key_file_path(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let from_xdg = xdg_config_home
        .filter(|value| !value.is_empty())
        .map(|value| Path::new(value).join("velo").join("subsonic.key"));
    from_xdg.or_else(|| {
        home.filter(|value| !value.is_empty()).map(|value| {
            Path::new(value)
                .join(".config")
                .join("velo")
                .join("subsonic.key")
        })
    })
}

/// Resolves the credentials for one server.
///
/// Order: `api_key` from the config, then the environment value (an empty
/// value counts as unset), then the key file. The URL is normalized here
/// (trailing slash removed) so the client can append `/rest/…` to it.
///
/// # Errors
///
/// Fails with [`Error::Credentials`] when the user is empty, or when no
/// secret is configured and the key file is missing, malformed or empty.
#[allow(
    clippy::too_many_arguments,
    reason = "every argument is one input the binary owns at the edge"
)]
pub fn resolve(
    url: &str,
    user: &str,
    api_key: Option<&str>,
    env_secret: Option<&OsStr>,
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<Credentials, Error> {
    if user.trim().is_empty() {
        return Err(Error::Credentials(
            "no subsonic user set — set `user` in the [subsonic] table".to_owned(),
        ));
    }

    let (secret, warnings) = if let Some(key) = api_key.filter(|key| !key.trim().is_empty()) {
        (key.trim().to_owned(), Vec::new())
    } else if let Some(secret) = env_secret.filter(|value| !value.is_empty()) {
        (secret.to_string_lossy().trim().to_owned(), Vec::new())
    } else {
        let Some(path) = key_file_path(xdg_config_home, home) else {
            return Err(Error::Credentials(
                "no subsonic secret found — set `api_key` in the [subsonic] table, or \
                 $VELO_SUBSONIC_SECRET, or write the password to ~/.config/velo/subsonic.key"
                    .to_owned(),
            ));
        };
        read_key_file(&path)?
    };

    Ok(Credentials {
        url: url.trim_end_matches('/').to_owned(),
        user: user.to_owned(),
        secret,
        warnings,
    })
}
