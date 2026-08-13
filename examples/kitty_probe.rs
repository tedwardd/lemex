//! Live kitty-graphics probe: renders a given image file through the real
//! `render_file` path and sleeps, so the result can be screenshotted.
//!
//! Usage: `cargo run --example kitty_probe -- <image> [placement]`
//! placement: `rect` (default) | `cols` | `native`

use std::io::Write;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = PathBuf::from(args.get(1).expect("usage: kitty_probe <image> [placement]"));
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
    let _ = std::fs::write("/tmp/kitty_probe_diag.txt", diag);
    eprintln!(
        "placing {} as {:?}; cell_px={:?}",
        path.display(),
        placement,
        cell_px
    );
    let bytes = lemmy::media::kitty::render_file(&path, placement).expect("render_file");
    let mut stdout = std::io::stdout();
    // Mirror the app: place at row 4, col 1 (image drawn at the cursor when
    // the final chunk arrives, C=1 keeps the cursor put).
    let _ = stdout.write_all(b"\x1b[4;1H");
    let _ = stdout.flush();
    let _ = stdout.write_all(&bytes);
    let _ = stdout.flush();
    eprintln!("rendered; sleeping 30s");
    std::thread::sleep(std::time::Duration::from_secs(30));
}
