use std::{fmt, path::PathBuf};
use url::Url;

use super::profile::ProfileId;

/// A media resource referenced by a Lemmy object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaRef {
    pub url: Url,
    pub mime_type: Option<String>,
    pub alt_text: Option<String>,
}

impl MediaRef {
    pub fn new(url: Url) -> Self {
        Self {
            url,
            mime_type: None,
            alt_text: None,
        }
    }
}

/// Stable identifier for one download attempt in the current session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DownloadId(pub u64);

impl DownloadId {
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl From<u64> for DownloadId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for DownloadId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Lifecycle of a download attempt. `Prompting` means a collision policy of
/// "prompt" found an existing file and is waiting for an overwrite/keep decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadStatus {
    Pending,
    Downloading { received: u64, total: Option<u64> },
    Prompting,
    Completed,
    Cancelled,
    Failed(String),
}

impl DownloadStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, DownloadStatus::Completed | DownloadStatus::Cancelled | DownloadStatus::Failed(_))
    }
}

impl fmt::Display for DownloadStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadStatus::Pending => formatter.write_str("pending"),
            DownloadStatus::Downloading { received, total: Some(total) } => write!(formatter, "downloading {received}/{total}"),
            DownloadStatus::Downloading { received, total: None } => write!(formatter, "downloading {received} bytes"),
            DownloadStatus::Prompting => formatter.write_str("prompting"),
            DownloadStatus::Completed => formatter.write_str("completed"),
            DownloadStatus::Cancelled => formatter.write_str("cancelled"),
            DownloadStatus::Failed(_) => formatter.write_str("failed"),
        }
    }
}

/// A media item downloaded during the current application session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadRecord {
    pub id: DownloadId,
    pub media: MediaRef,
    /// Requested filename derived from the source URL.
    pub filename: String,
    /// Resolved MIME type (server metadata, response header, then filename).
    pub mime_type: Option<String>,
    pub profile: ProfileId,
    pub instance_url: Url,
    /// Unix seconds when the attempt was created.
    pub requested_at: i64,
    /// Final local path; the file exists only once status is `Completed`.
    pub local_path: PathBuf,
    pub status: DownloadStatus,
    /// True after the user confirmed deletion of the local file.
    pub local_file_deleted: bool,
}

impl DownloadRecord {
    pub fn new(
        id: DownloadId,
        media: MediaRef,
        filename: impl Into<String>,
        profile: ProfileId,
        instance_url: Url,
        requested_at: i64,
        local_path: PathBuf,
    ) -> Self {
        Self {
            id,
            media,
            filename: filename.into(),
            mime_type: None,
            profile,
            instance_url,
            requested_at,
            local_path,
            status: DownloadStatus::Pending,
            local_file_deleted: false,
        }
    }
}
