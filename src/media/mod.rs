use std::{fs, path::PathBuf};

use crate::error::{AppError, Result};

pub mod download;
pub mod mailcap;
pub mod mime;

pub use crate::domain::{DownloadId, DownloadRecord, DownloadStatus, MediaRef};
pub use download::{
    CollisionPolicy, DownloadEvent, DownloadManager, DownloadRequest, SessionDownloadHistory,
    filename_for, probe_content_type,
};
pub use mailcap::{MailcapEntry, build_argv, find_entry, load_entries, parse_mailcap};
pub use mime::{
    MediaHandler, MediaPolicyConfig, extension_for_mime, is_image, mime_from_content_type,
    mime_from_filename, resolve_mime,
};

/// Name of the exclusively-owned subdirectory under the system temp
/// directory. Everything the client downloads for external media handlers
/// lives inside it, so a `--clean-temp` sweep can remove the whole subtree
/// without tracking individual files.
const SCRATCH_SUBDIR: &str = "lemmy-client";

/// Scratch directory for media files downloaded for external handlers.
///
/// Nested under the system temp directory rather than written straight into
/// the temp root. The base directory complies with the POSIX convention
/// (tempnam/tmpfile): the value of `$TMPDIR` is honored when set and usable,
/// falling back to `/tmp` — `std::env::temp_dir()` implements exactly that
/// resolution, including ignoring an empty `$TMPDIR`.
pub fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(SCRATCH_SUBDIR)
}

/// Create the scratch directory and return it; failures surface as storage
/// errors so a broken `$TMPDIR` cannot silently scatter files elsewhere.
pub fn ensure_scratch_dir() -> Result<PathBuf> {
    let directory = scratch_dir();
    fs::create_dir_all(&directory).map_err(|error| {
        AppError::Storage(format!(
            "cannot create temp media directory {}: {error}",
            directory.display()
        ))
    })?;
    Ok(directory)
}

/// Remove the client's temp media subtree (crash leftovers, stale handler
/// files). The subtree is exclusively owned by the client, so removing it is
/// safe without per-file tracking; a missing directory is not an error.
pub fn clean_scratch_dir() -> Result<()> {
    let directory = scratch_dir();
    match fs::remove_dir_all(&directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::Storage(format!(
            "cannot clean temp media directory {}: {error}",
            directory.display()
        ))),
    }
}

/// Whether the host the client runs on has a display for external GUI
/// handlers. Over plain SSH without X11 forwarding both are unset, and a
/// spawned `xdg-open` would fail invisibly.
pub fn environment_has_display(display: Option<&str>, wayland_display: Option<&str>) -> bool {
    display.is_some_and(|value| !value.is_empty())
        || wayland_display.is_some_and(|value| !value.is_empty())
}

/// Whether the client runs inside an SSH login session. `sshd` sets these
/// variables for interactive sessions, and tmux inherits them from the
/// session that started the server, so a tmux-over-SSH setup is detected.
/// Media handlers then run on the remote host, which the user should be
/// told about.
pub fn environment_is_ssh(
    ssh_connection: Option<&str>,
    ssh_client: Option<&str>,
    ssh_tty: Option<&str>,
) -> bool {
    [ssh_connection, ssh_client, ssh_tty]
        .into_iter()
        .flatten()
        .any(|value| !value.is_empty())
}
