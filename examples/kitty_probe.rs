//! Live kitty-graphics probe: renders an image through the real
//! `render_file` path inside the user's terminal and sleeps, so the
//! result can be inspected by eye.
//!
//! Usage:
//!   cargo run --example kitty_probe                 # generates a test image
//!   cargo run --example kitty_probe -- <image>      # rect placement
//!   cargo run --example kitty_probe -- <image> cols # columns-only
//!   cargo run --example kitty_probe -- <image> native

use std::io::Write;
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = match args.get(1) {
        Some(p) if Path::new(p).exists() => PathBuf::from(p),
        Some(p) => {
            eprintln!("{p}: file not found; run without arguments to generate a test image");
            std::process::exit(1);
        }
        None => {
            let path = PathBuf::from("/tmp/kitty_probe_test.png");
            match write_test_png(&path, 800, 600, (220, 40, 40)) {
                Ok(()) => eprintln!("no image given: wrote a solid 800x600 test image to {path:?}"),
                Err(error) => {
                    eprintln!("cannot write test image: {error}");
                    std::process::exit(1);
                }
            }
            path
        }
    };

    let placement = match args.get(2).map(String::as_str) {
        Some("cols") => lemmy::media::kitty::ImagePlacement::FitColumns { cols: 44 },
        Some("native") => lemmy::media::kitty::ImagePlacement::Native,
        _ => {
            let area = (44u16, 33u16);
            let cell_px = lemmy::media::kitty::cell_pixels();
            match lemmy::media::kitty::image_dimensions(&path) {
                Some(image) => {
                    let (cols, rows) = lemmy::media::kitty::fit_cells(image, area, cell_px);
                    lemmy::media::kitty::ImagePlacement::Rect { cols, rows }
                }
                None => lemmy::media::kitty::ImagePlacement::Native,
            }
        }
    };

    let cell_px = lemmy::media::kitty::cell_pixels();
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let tty_size = if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0 {
        format!("{}x{}", ws.ws_col, ws.ws_row)
    } else {
        "?".into()
    };
    let dims = lemmy::media::kitty::image_dimensions(&path)
        .map(|d| format!("{d:?}"))
        .unwrap_or_else(|| "?".into());
    let diag = format!(
        "image={dims} placement={placement:?} cell_px={cell_px:?} terminal_size={tty_size}\n"
    );
    let _ = std::fs::write("/tmp/kitty_probe_diag.txt", &diag);
    eprint!("{diag}");

    let bytes = match lemmy::media::kitty::render_file(&path, placement) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot render {}: {error}", path.display());
            std::process::exit(1);
        }
    };
    let mut stdout = std::io::stdout();
    // Mirror the app: the image is drawn at the cursor when the final chunk
    // arrives (C=1 keeps the cursor put).
    let _ = stdout.write_all(b"\x1b[4;1H");
    let _ = stdout.flush();
    let _ = stdout.write_all(&bytes);
    let _ = stdout.flush();
    eprintln!("rendered; image stays 30s, then the terminal is cleared");
    std::thread::sleep(std::time::Duration::from_secs(30));
    let _ = stdout.write_all(lemmy::media::kitty::clear_images());
    let _ = stdout.flush();
}

/// Minimal dependency-free PNG writer (truecolor, uncompressed deflate
/// blocks), valid enough for any kitty-graphics terminal to decode.
fn write_test_png(path: &Path, width: u32, height: u32, rgb: (u8, u8, u8)) -> std::io::Result<()> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc_input = Vec::with_capacity(4 + data.len());
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    }
    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit truecolor
    chunk(&mut png, b"IHDR", &ihdr);

    let stride = 1 + 3 * width as usize; // filter byte + RGB rows
    let mut raw = Vec::with_capacity(stride * height as usize);
    for _ in 0..height {
        raw.push(0);
        raw.extend(std::iter::repeat_n([rgb.0, rgb.1, rgb.2], width as usize).flatten());
    }
    // zlib: header, stored deflate blocks (max 65535 bytes each), adler32.
    let mut z = Vec::new();
    z.extend_from_slice(&[0x78, 0x01]);
    let mut off = 0;
    loop {
        let len = (raw.len() - off).min(65535);
        let last = u8::from(off + len == raw.len());
        z.push(last);
        z.extend_from_slice(&(len as u16).to_le_bytes());
        z.extend_from_slice(&(!(len as u16)).to_le_bytes());
        z.extend_from_slice(&raw[off..off + len]);
        off += len;
        if last == 1 {
            break;
        }
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    std::fs::write(path, png)
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
