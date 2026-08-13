use std::{fs, path::Path};

use crate::error::{AppError, Result};

/// Detect whether the running terminal advertises Kitty graphics protocol
/// support. This is a conservative environment check (the `TERM` value,
/// `TERM_PROGRAM`, or the presence of `KITTY_WINDOW_ID`); it never writes to
/// the terminal. Both Kitty and Ghostty advertise the graphics protocol;
/// Ghostty neither names itself "kitty" in `TERM` nor sets
/// `KITTY_WINDOW_ID`, so `TERM_PROGRAM` is checked too.
pub fn detect_support() -> bool {
    detect_support_in(
        std::env::var("TERM").ok().as_deref(),
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var_os("KITTY_WINDOW_ID").is_some(),
        std::env::var_os("TMUX").is_some(),
    )
}

/// Pure capability decision: Kitty graphics require a terminal that
/// implements the protocol (Kitty or Ghostty), and tmux never forwards the
/// graphics protocol — even a `TERM` that claims kitty (for example a
/// `default-terminal xterm-kitty` tmux configuration) cannot render inline
/// through a tmux pane.
pub fn detect_support_in(
    term: Option<&str>,
    term_program: Option<&str>,
    kitty_window_id: bool,
    tmux: bool,
) -> bool {
    if tmux {
        return false;
    }
    let graphics_capable = term
        .is_some_and(|term| term.contains("kitty") || term.contains("ghostty"))
        || term_program.is_some_and(|program| {
            program.eq_ignore_ascii_case("kitty") || program.eq_ignore_ascii_case("ghostty")
        });
    graphics_capable || kitty_window_id
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

/// Produce the escape sequences that transmit and place a raster file through
/// the Kitty graphics protocol. `cells` optionally scales the display to the
/// given column/row rectangle (aspect fitted by the terminal); the placement
/// happens at the cursor position when the final chunk arrives, so the caller
/// moves the cursor first. `C=1` keeps the cursor from jumping after the
/// placement, and `q=2` suppresses error responses. `a=T` both transmits and
/// displays, so no separate `a=p` is needed.
pub fn render_file(path: &Path, cells: Option<(u16, u16)>) -> Result<Vec<u8>> {
    let bytes = fs::read(path)
        .map_err(|error| AppError::Media(format!("cannot read {}: {error}", path.display())))?;
    if bytes.is_empty() {
        return Err(AppError::Media("cannot render an empty media file".into()));
    }
    let format = format_code(path);
    let placement = match cells {
        Some((cols, rows)) => format!(",c={cols},r={rows},C=1"),
        None => ",C=1".to_owned(),
    };
    let encoded = base64_encode(&bytes);
    let chunk_size = 4096usize;
    let mut out = Vec::with_capacity(encoded.len() + 128);
    let mut offset = 0;
    while offset < encoded.len() {
        let end = (offset + chunk_size).min(encoded.len());
        let more = if end < encoded.len() { 1 } else { 0 };
        let control = if offset == 0 {
            format!("a=T,f={format},q=2{placement},m={more}")
        } else {
            format!("m={more}")
        };
        out.extend_from_slice(b"\x1b_G");
        out.extend_from_slice(control.as_bytes());
        out.extend_from_slice(b";");
        out.extend_from_slice(&encoded.as_bytes()[offset..end]);
        out.extend_from_slice(b"\x1b\\");
        offset = end;
    }
    Ok(out)
}

/// Escape sequence that deletes every kitty graphics placement/image,
/// returning the terminal to its plain text state.
pub fn clear_images() -> &'static [u8] {
    b"\x1b_Ga=d\x1b\\"
}

/// Pixel dimensions of PNG, GIF, and JPEG files read from their headers, so
/// the caller can scale the display rectangle to the image's aspect ratio.
pub fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    let bytes = fs::read(path).ok()?;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return Some((width, height));
    }
    if bytes.starts_with(b"GIF8") && bytes.len() >= 10 {
        let width = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let height = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return Some((width, height));
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        // Walk the JPEG segment markers to the first SOF0/SOF2 frame header.
        let mut offset = 2usize;
        while offset + 9 < bytes.len() {
            if bytes[offset] != 0xff {
                return None;
            }
            let marker = bytes[offset + 1];
            if marker == 0xd8 || (0xd0..=0xd9).contains(&marker) {
                offset += 2;
                continue;
            }
            let length = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]) as usize;
            if matches!(marker, 0xc0..=0xc3) && length >= 7 {
                let height = u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]) as u32;
                let width = u16::from_be_bytes([bytes[offset + 7], bytes[offset + 8]]) as u32;
                return Some((width, height));
            }
            offset += 2 + length;
        }
    }
    None
}

/// Largest cell rectangle with the image's aspect ratio that fits inside
/// `area`, so the terminal scales the image without distortion.
pub fn fit_cells(image: (u32, u32), area: (u16, u16)) -> (u16, u16) {
    let (width, height) = (image.0.max(1) as f64, image.1.max(1) as f64);
    let (max_cols, max_rows) = (area.0.max(1) as f64, area.1.max(1) as f64);
    let scale = (max_cols / width).min(max_rows / height);
    (
        (width * scale).floor().max(1.0) as u16,
        (height * scale).floor().max(1.0) as u16,
    )
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

#[cfg(test)]
mod tests {
    use super::{
        clear_images, detect_support_in, environment_has_display, environment_is_ssh, fit_cells,
        image_dimensions,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn clear_images_deletes_all_placements() {
        assert_eq!(clear_images(), b"\x1b_Ga=d\x1b\\");
    }

    #[test]
    fn image_dimensions_reads_headers() {
        let png = [
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0,
            0, 0, 40, 0, 0, 0, 30, 8, 6,
        ];
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/kitty-test.png");
        std::fs::create_dir_all(path.parent().unwrap()).ok();
        std::fs::write(&path, png).ok();
        assert_eq!(image_dimensions(&path), Some((40, 30)));
        let _ = std::fs::remove_file(&path);
        assert_eq!(image_dimensions(Path::new("/nonexistent")), None);
    }

    #[test]
    fn fit_cells_preserves_aspect_inside_the_area() {
        // 400x200 image into a 30x10 area: fit by rows (10), cols = 20.
        assert_eq!(fit_cells((400, 200), (30, 10)), (20, 10));
        // Wide image into a narrow area: fit by cols.
        assert_eq!(fit_cells((400, 100), (30, 10)), (30, 7));
        // Extreme aspect ratios still occupy at least one cell per side.
        assert_eq!(fit_cells((5000, 1), (30, 10)), (30, 1));
    }

    #[test]
    fn render_file_places_once_with_scale_and_no_cursor_move() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/kitty-render.png");
        let png = [
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0,
            0, 0, 4, 0, 0, 0, 2, 8, 6,
        ];
        std::fs::write(&path, png).ok();
        let bytes = super::render_file(&path, Some((20, 10))).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("\x1b_Ga=T,f=100,q=2,c=20,r=10,C=1,m="));
        assert!(
            !text.contains("a=p"),
            "a=T already places the image; a separate a=p would double-place"
        );
        assert!(
            text.contains("m=0"),
            "the final chunk must end the transmission"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ssh_session_is_detected_from_sshd_environment() {
        assert!(environment_is_ssh(
            Some("192.168.1.5 51234 10.0.0.2 22"),
            None,
            None
        ));
        assert!(environment_is_ssh(None, Some("192.168.1.5 51234 22"), None));
        assert!(environment_is_ssh(None, None, Some("/dev/pts/3")));
        assert!(!environment_is_ssh(None, None, None));
        assert!(!environment_is_ssh(Some(""), Some(""), None));
    }

    #[test]
    fn kitty_is_unsupported_inside_tmux_even_with_a_kitty_term() {
        assert!(
            !detect_support_in(Some("xterm-kitty"), None, true, true),
            "tmux never forwards the graphics protocol"
        );
        assert!(detect_support_in(Some("xterm-kitty"), None, false, false));
        assert!(detect_support_in(None, None, true, false));
        assert!(!detect_support_in(
            Some("screen-256color"),
            None,
            false,
            false
        ));
        assert!(!detect_support_in(None, None, false, false));
    }

    #[test]
    fn ghostty_is_detected_as_graphics_capable() {
        // Ghostty neither says "kitty" in TERM nor sets KITTY_WINDOW_ID, but
        // it implements the kitty graphics protocol and advertises itself
        // through TERM and TERM_PROGRAM.
        assert!(detect_support_in(Some("xterm-ghostty"), None, false, false));
        assert!(detect_support_in(
            Some("xterm-256color"),
            Some("Ghostty"),
            false,
            false
        ));
        assert!(
            !detect_support_in(Some("xterm-256color"), Some("Ghostty"), false, true,),
            "tmux still blocks Ghostty graphics"
        );
    }

    #[test]
    fn display_detection_requires_x11_or_wayland() {
        assert!(environment_has_display(Some(":0"), None));
        assert!(environment_has_display(None, Some("wayland-0")));
        assert!(!environment_has_display(None, None));
        assert!(!environment_has_display(Some(""), None));
    }
}
