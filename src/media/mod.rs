pub mod download;
pub mod kitty;
pub mod mailcap;
pub mod mime;

pub use crate::domain::{DownloadId, DownloadRecord, DownloadStatus, MediaRef};
pub use download::{
    CollisionPolicy, DownloadEvent, DownloadManager, DownloadRequest, SessionDownloadHistory,
    filename_for, probe_content_type,
};
pub use mailcap::{MailcapEntry, build_argv, find_entry, load_entries, parse_mailcap};
pub use mime::{
    MediaHandler, MediaPolicyConfig, TerminalCapabilities, extension_for_mime, is_image,
    mime_from_content_type, mime_from_filename, resolve_mime,
};
