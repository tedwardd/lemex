use std::path::PathBuf;
use url::Url;

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

/// A media item downloaded during the current application session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadRecord {
    pub media: MediaRef,
    pub local_path: PathBuf,
}

impl DownloadRecord {
    pub fn new(media: MediaRef, local_path: PathBuf) -> Self {
        Self { media, local_path }
    }
}
