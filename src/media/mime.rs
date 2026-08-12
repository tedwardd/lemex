use std::collections::HashMap;

use url::Url;

use crate::domain::MediaRef;

use super::mailcap::{find_entry, MailcapEntry};

/// Capabilities reported by the terminal that the client runs in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TerminalCapabilities {
    pub kitty: bool,
}

/// What the client should do with a media reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaHandler {
    /// Run a parsed mailcap entry (or the default external opener) with a
    /// safely constructed argv.
    Mailcap { command: String },
    /// Render the media inline through the Kitty graphics protocol.
    KittyInline,
    /// Run an explicitly configured handler command.
    External { command: String },
    /// No handler applies; only metadata is available.
    MetadataOnly,
}

/// Policy that turns a media reference plus terminal capabilities into a handler.
///
/// Mailcap is the default external handler. Kitty is selected only when both
/// `kitty_enabled` and the terminal capability check succeed. Explicitly
/// configured handlers win over mailcap; unsupported types degrade to
/// metadata-only handling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaPolicyConfig {
    pub kitty_enabled: bool,
    pub mailcap_enabled: bool,
    /// MIME type -> command template (explicit user configuration).
    pub handlers: HashMap<String, String>,
    /// Entries parsed from mailcap files.
    pub mailcap_entries: Vec<MailcapEntry>,
}

/// Fallback mailcap-style opener used when no mailcap entry matches and no
/// explicit handler is configured. Equivalent to the mailcap default action.
pub const DEFAULT_MAILCAP_COMMAND: &str = "xdg-open %s";

impl Default for MediaPolicyConfig {
    fn default() -> Self {
        Self {
            kitty_enabled: false,
            mailcap_enabled: true,
            handlers: HashMap::new(),
            mailcap_entries: Vec::new(),
        }
    }
}

impl MediaPolicyConfig {
    pub fn from_config(config: &crate::config::MediaConfig) -> Self {
        Self {
            kitty_enabled: config.kitty_enabled,
            mailcap_enabled: config.mailcap_enabled,
            handlers: config.handlers.clone(),
            mailcap_entries: super::mailcap::load_entries(),
        }
    }

    pub fn select(&self, media: &MediaRef, capabilities: &TerminalCapabilities) -> MediaHandler {
        let mime = resolve_mime(media, None).unwrap_or_default();
        if is_image(&mime) && self.kitty_enabled && capabilities.kitty {
            return MediaHandler::KittyInline;
        }
        if let Some(command) = self.handlers.get(&mime) {
            return MediaHandler::External { command: command.clone() };
        }
        if self.mailcap_enabled && !mime.is_empty() {
            let command = find_entry(&self.mailcap_entries, &mime)
                .map(|entry| entry.command.clone())
                .unwrap_or_else(|| DEFAULT_MAILCAP_COMMAND.to_owned());
            return MediaHandler::Mailcap { command };
        }
        MediaHandler::MetadataOnly
    }
}

/// Resolve a MIME type in precedence order: server metadata on the media
/// reference, the HTTP response `Content-Type` header, then the URL filename.
pub fn resolve_mime(media: &MediaRef, content_type: Option<&str>) -> Option<String> {
    if let Some(metadata) = media.mime_type.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        return Some(metadata.to_owned());
    }
    if let Some(header) = content_type.and_then(mime_from_content_type) {
        return Some(header);
    }
    mime_from_filename(&media.url)
}

/// Parse a `Content-Type` header value, dropping parameters such as charset.
pub fn mime_from_content_type(header: &str) -> Option<String> {
    let value = header.split(';').next()?.trim().to_ascii_lowercase();
    if value.is_empty() { None } else { Some(value) }
}

/// Guess a MIME type from the URL path's final extension.
pub fn mime_from_filename(url: &Url) -> Option<String> {
    let name = url.path_segments()?.last()?;
    let extension = name.rsplit('.').next()?;
    if extension == name && !name.contains('.') {
        return None;
    }
    let mime = match extension.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "xml" => "application/xml",
        "json" => "application/json",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "epub" => "application/epub+zip",
        _ => return None,
    };
    Some(mime.to_owned())
}

pub fn is_image(mime: &str) -> bool {
    mime.starts_with("image/")
}

/// Preferred file extension for a MIME type, used when a URL has no extension.
pub fn extension_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/avif" => Some("avif"),
        "image/svg+xml" => Some("svg"),
        "video/mp4" => Some("mp4"),
        "video/webm" => Some("webm"),
        "audio/mpeg" => Some("mp3"),
        "audio/ogg" => Some("ogg"),
        "audio/wav" => Some("wav"),
        "audio/flac" => Some("flac"),
        "application/pdf" => Some("pdf"),
        "text/plain" => Some("txt"),
        "text/markdown" => Some("md"),
        "text/html" => Some("html"),
        "text/csv" => Some("csv"),
        "application/json" => Some("json"),
        "application/zip" => Some("zip"),
        "application/gzip" => Some("gz"),
        _ => None,
    }
}
