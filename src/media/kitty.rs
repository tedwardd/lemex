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

#[cfg(test)]
mod tests {
    use super::{detect_support_in, environment_has_display, environment_is_ssh};

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
