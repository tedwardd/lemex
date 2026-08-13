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
