use std::{fs, path::Path};

use crate::error::{AppError, Result};

/// Detect whether the running terminal advertises Kitty graphics protocol
/// support. This is a conservative environment check (the `TERM` value or the
/// presence of `KITTY_WINDOW_ID`); it never writes to the terminal.
pub fn detect_support() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    if term.contains("kitty") {
        return true;
    }
    std::env::var("KITTY_WINDOW_ID").is_ok()
}

/// Produce the escape sequences that transmit and place a raster file through
/// the Kitty graphics protocol. The file is read, base64-encoded, chunked into
/// `a=T` transmissions, and finally placed with `a=p`.
pub fn render_file(path: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(path)
        .map_err(|error| AppError::Media(format!("cannot read {}: {error}", path.display())))?;
    if bytes.is_empty() {
        return Err(AppError::Media("cannot render an empty media file".into()));
    }
    let format = format_code(path);
    let encoded = base64_encode(&bytes);
    let chunk_size = 4096usize;
    let mut out = Vec::with_capacity(encoded.len() + 128);
    let mut offset = 0;
    while offset < encoded.len() {
        let end = (offset + chunk_size).min(encoded.len());
        let more = if end < encoded.len() { 1 } else { 0 };
        out.extend_from_slice(b"\x1b_G");
        out.extend_from_slice(format!("a=T,f={format},m={more};").as_bytes());
        out.extend_from_slice(&encoded.as_bytes()[offset..end]);
        out.extend_from_slice(b"\x1b\\");
        offset = end;
    }
    // Place the transmitted image at the cursor without moving it.
    out.extend_from_slice(b"\x1b_Ga=p,q=2\x1b\\");
    Ok(out)
}

/// Kitty format code for common raster types; PNG is the safe default.
fn format_code(path: &Path) -> u32 {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" => 100,
        "jpg" | "jpeg" => 101,
        "gif" => 102,
        _ => 100,
    }
}

/// Standard base64 encoding with padding; kept dependency-free.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let combined = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(combined >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(combined >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(combined >> 6) as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[combined as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}
