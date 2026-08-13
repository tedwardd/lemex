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

/// How to place a transmitted image in the terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImagePlacement {
    /// Scale to an exact cell rectangle whose pixel aspect matches the
    /// source (used when the cell pixel size is known).
    Rect { cols: u16, rows: u16 },
    /// Fit to these columns; the terminal derives the rows from the source
    /// image aspect ratio, which the protocol guarantees to be
    /// distortion-free (used when the cell pixel size is unknown).
    FitColumns { cols: u16 },
    /// Render at native size at the cursor (used when the image dimensions
    /// cannot be determined).
    Native,
}

/// Produce the escape sequences that transmit and place a raster file through
/// the Kitty graphics protocol. The placement happens at the cursor position
/// when the final chunk arrives, so the caller moves the cursor first. `C=1`
/// keeps the cursor from jumping after the placement, and `q=2` suppresses
/// error responses. `a=T` both transmits and displays, so no separate `a=p`
/// is needed.
pub fn render_file(path: &Path, placement: ImagePlacement) -> Result<Vec<u8>> {
    let bytes = fs::read(path)
        .map_err(|error| AppError::Media(format!("cannot read {}: {error}", path.display())))?;
    if bytes.is_empty() {
        return Err(AppError::Media("cannot render an empty media file".into()));
    }
    let format = format_code(path);
    let placement = match placement {
        ImagePlacement::Rect { cols, rows } => format!(",c={cols},r={rows},C=1"),
        ImagePlacement::FitColumns { cols } => format!(",c={cols},C=1"),
        ImagePlacement::Native => ",C=1".to_owned(),
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

/// Cell pixel dimensions from `TIOCGWINSZ` (the kitty protocol docs require
/// this to size images correctly). Returns `(0, 0)` when the terminal does
/// not report pixel sizes — including when only one axis is reported, since
/// a partial value would make the cell aspect nonsense and distort the
/// image. Callers then fall back to the distortion-free columns-only
/// placement.
pub fn cell_pixels() -> (u32, u32) {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0
        || unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
    if !ok {
        return (0, 0);
    }
    let (cols, rows) = (u32::from(size.ws_col), u32::from(size.ws_row));
    if cols == 0 || rows == 0 {
        return (0, 0);
    }
    let (cell_w, cell_h) = (
        u32::from(size.ws_xpixel) / cols,
        u32::from(size.ws_ypixel) / rows,
    );
    if cell_w == 0 || cell_h == 0 {
        return (0, 0);
    }
    (cell_w, cell_h)
}

/// Largest cell rectangle whose pixel aspect ratio matches the image's,
/// fitting inside `area` cells and accounting for non-square terminal cells
/// (the kitty protocol requires the cell pixel size for correct sizing).
///
/// The ratio is solved exactly as the reduced fraction
/// `rows/cols = (cell_w * h) / (cell_h * w)` when the smallest integer
/// solution fits the area. The exact solution only scales up by whole
/// units, so when even one unit does not fit — common for 16:9/21:9
/// images in a half-width pane — a float contain-fit is used instead:
/// the limiting axis is floored and the other derived from the image
/// ratio, keeping the pixel rectangle within half a cell of the source
/// aspect. `cell_px == (0, 0)` assumes square cells.
pub fn fit_cells(image: (u32, u32), area: (u16, u16), cell_px: (u32, u32)) -> (u16, u16) {
    let cell_w = u64::from(cell_px.0.max(1));
    let cell_h = u64::from(cell_px.1.max(1));
    let (width, height) = (u64::from(image.0.max(1)), u64::from(image.1.max(1)));
    let (max_cols, max_rows) = (u64::from(area.0.max(1)), u64::from(area.1.max(1)));

    let (mut rows_unit, mut cols_unit) = (cell_w * height, cell_h * width);
    let divisor = gcd(rows_unit, cols_unit);
    rows_unit /= divisor;
    cols_unit /= divisor;

    let units = (max_cols / cols_unit).min(max_rows / rows_unit);
    if units > 0 {
        return ((units * cols_unit) as u16, (units * rows_unit) as u16);
    }

    // One exact unit does not fit the area: contain-fit in cell units,
    // floor the limiting axis and derive the other from the image ratio.
    let (width_cells, height_cells) = (width as f64 / cell_w as f64, height as f64 / cell_h as f64);
    let scale = (max_cols as f64 / width_cells).min(max_rows as f64 / height_cells);
    if width_cells * scale >= height_cells * scale {
        // Width binds: floor cols, derive rows from the image ratio.
        let cols = (width_cells * scale).floor().max(1.0) as u16;
        let rows = (f64::from(cols) * height_cells / width_cells)
            .round()
            .clamp(1.0, max_rows as f64) as u16;
        (cols, rows)
    } else {
        // Height binds: floor rows, derive cols from the image ratio.
        let rows = (height_cells * scale).floor().max(1.0) as u16;
        let cols = (f64::from(rows) * width_cells / height_cells)
            .round()
            .clamp(1.0, max_cols as f64) as u16;
        (cols, rows)
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
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

    fn pixel_ratio(cols: u16, rows: u16, cell: (u32, u32)) -> f64 {
        (f64::from(cols) * f64::from(cell.0)) / (f64::from(rows) * f64::from(cell.1))
    }

    #[test]
    fn fit_cells_preserves_aspect_inside_the_area() {
        // Square cells: a 2:1 image into a 30x10 area.
        assert_eq!(fit_cells((400, 200), (30, 10), (8, 8)), (20, 10));
        // Wide image into a narrow area: exact 4:1 within the grid.
        assert_eq!(fit_cells((400, 100), (30, 10), (8, 8)), (28, 7));
        // Extreme aspect ratios still occupy at least one cell per side.
        assert_eq!(fit_cells((5000, 1), (30, 10), (8, 8)), (30, 1));
    }

    #[test]
    fn fit_cells_uses_cell_pixel_aspect() {
        // A 1:1 image in a 2:1 cell (8x16 px) occupies a 1:1 pixel rect.
        let (cols, rows) = fit_cells((100, 100), (20, 10), (8, 16));
        assert_eq!((cols, rows), (20, 10));
        assert!((pixel_ratio(cols, rows, (8, 16)) - 1.0).abs() < 1e-9);

        // A 4:1 image in the same cells keeps 4:1 in pixels, exactly.
        let (cols, rows) = fit_cells((400, 100), (30, 10), (8, 16));
        assert_eq!((cols, rows), (24, 3));
        assert!((pixel_ratio(cols, rows, (8, 16)) - 4.0).abs() < 1e-9);

        // A tall 1:4 image stays 1:4.
        let (cols, rows) = fit_cells((100, 400), (30, 10), (8, 16));
        assert!((pixel_ratio(cols, rows, (8, 16)) - 0.25).abs() < 1e-9);
        assert!(cols <= 30 && rows <= 10, "the rect stays inside the area");
    }

    #[test]
    fn fit_cells_never_collapses_to_a_strip_when_the_exact_unit_does_not_fit() {
        // Real Ghostty cells (10x21 px) in a 92x47 window: a 16:9 image's
        // reduced unit is 56 cols wide, wider than the ~44-col pane, so the
        // exact solution does not fit. It must fall back to a float
        // contain-fit filling the pane — not a 1-row strip, which Ghostty
        // stretches into the "squashed" image.
        let (cols, rows) = fit_cells((1920, 1080), (44, 33), (10, 21));
        assert_eq!((cols, rows), (44, 12));
        assert!(rows >= 10, "must not collapse to a 1-row strip");
        assert!(cols <= 44 && rows <= 33, "the rect stays inside the area");
        let ratio = pixel_ratio(cols, rows, (10, 21));
        assert!(
            (ratio - 16.0 / 9.0).abs() < 0.1,
            "pixel rectangle ratio {ratio} must stay close to 16:9"
        );

        // 4:3 images still hit the exact path.
        assert_eq!(fit_cells((800, 600), (44, 33), (10, 21)), (42, 15));

        // Extreme ultrawide also uses the float fallback, never a strip.
        let (cols, rows) = fit_cells((3440, 1440), (44, 33), (10, 21));
        assert!(rows >= 8 && cols <= 44, "ultrawide stays a wide rect");
        let ratio = pixel_ratio(cols, rows, (10, 21));
        assert!((ratio - 3440.0 / 1440.0).abs() < 0.15);
    }

    #[test]
    fn render_file_places_once_with_scale_and_no_cursor_move() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/kitty-render.png");
        let png = [
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0,
            0, 0, 4, 0, 0, 0, 2, 8, 6,
        ];
        std::fs::write(&path, png).ok();
        let bytes =
            super::render_file(&path, super::ImagePlacement::Rect { cols: 20, rows: 10 }).unwrap();
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

        // Unknown cell size: columns-only placement, rows derived by the
        // terminal from the source aspect (distortion-free per the spec).
        let bytes =
            super::render_file(&path, super::ImagePlacement::FitColumns { cols: 20 }).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("c=20,C=1") && !text.contains(",r="),
            "columns-only placement must not send rows"
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
