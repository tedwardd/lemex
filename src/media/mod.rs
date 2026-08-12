pub mod download;
pub mod kitty;
pub mod mailcap;
pub mod mime;

pub use crate::domain::{DownloadId, DownloadRecord, DownloadStatus, MediaRef};
pub use download::{
    filename_for, CollisionPolicy, DownloadEvent, DownloadManager, DownloadRequest,
    SessionDownloadHistory,
};
pub use mailcap::{build_argv, find_entry, load_entries, MailcapEntry, parse_mailcap};
pub use mime::{
    extension_for_mime, is_image, mime_from_content_type, mime_from_filename, resolve_mime,
    MediaHandler, MediaPolicyConfig, TerminalCapabilities,
};
