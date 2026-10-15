use base64::Engine;
use std::io::{IsTerminal, Write};

fn terminal_geometry() -> (u32, u32, u32, u32) {
    #[cfg(unix)]
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) == 0
            && size.ws_col > 0
            && size.ws_row > 0
        {
            return (
                size.ws_col as u32,
                size.ws_row as u32,
                (size.ws_xpixel as u32 / size.ws_col as u32).max(8),
                (size.ws_ypixel as u32 / size.ws_row as u32).max(18),
            );
        }
    }
    (80, 24, 10, 20)
}

fn display_size(w: u32, h: u32, geometry: (u32, u32, u32, u32)) -> (u32, u32, u32, u32) {
    let (cols, rows, cw, ch) = geometry;
    let max_w = cols.saturating_sub(2).clamp(1, 80) * cw;
    let max_h = (rows / 2).clamp(1, 24) * ch;
    let ratio = (max_w as f64 / w as f64)
        .min(max_h as f64 / h as f64)
        .min(1.0);
    let w = (w as f64 * ratio).floor().max(1.0) as u32;
    let h = (h as f64 * ratio).floor().max(1.0) as u32;
    (w, h, w.div_ceil(cw), h.div_ceil(ch))
}

fn emit_png(output: &mut impl Write, png: &[u8], cols: u32, rows: u32) -> std::io::Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    // 먼저 줄을 확보해야 그림이 뒤따르는 셸 프롬프트나 앞의 출력을 덮지 않는다.
    write!(output, "\r\n")?;
    for _ in 0..rows {
        write!(output, "\r\n")?;
    }
    write!(output, "\x1b[{rows}A")?;
    let count = encoded.len().div_ceil(4096);
    for (index, chunk) in encoded.as_bytes().chunks(4096).enumerate() {
        let more = u8::from(index + 1 < count);
        if index == 0 {
            write!(
                output,
                "\x1b_Ga=T,f=100,t=d,q=2,C=1,c={cols},r={rows},m={more};"
            )?;
        } else {
            write!(output, "\x1b_Gm={more},q=2;")?;
        }
        output.write_all(chunk)?;
        output.write_all(b"\x1b\\")?;
    }
    write!(output, "\x1b[{rows}B\r\n")?;
    output.flush()
}

pub(crate) fn run(args: &[std::ffi::OsString]) -> anyhow::Result<bool> {
    if args.first().is_none_or(|a| a != "--inline-image") {
        return Ok(false);
    }
    anyhow::ensure!(
        args.len() == 2,
        "usage: imgopen FILE (or imgopen --tab FILE)"
    );
    let stdout = std::io::stdout();
    anyhow::ensure!(stdout.is_terminal(), "이미지는 하네스의 이미지 첨부 도구로 보여 주세요. imgopen은 셸 터미널용이며, 별도 탭은 imgopen --tab FILE로 엽니다.");
    let path = std::path::Path::new(&args[1]);
    anyhow::ensure!(
        path.is_file(),
        "이미지 파일을 찾을 수 없습니다: {}",
        path.display()
    );
    let (w, h) = image::image_dimensions(path)?;
    anyhow::ensure!(
        w > 0 && h > 0 && u64::from(w) * u64::from(h) <= 64_000_000,
        "이미지가 너무 큽니다"
    );
    let (width, height, cols, rows) = display_size(w, h, terminal_geometry());
    let decoded = image::open(path)?.thumbnail(width, height);
    let mut png = std::io::Cursor::new(Vec::new());
    decoded.write_to(&mut png, image::ImageFormat::Png)?;
    emit_png(&mut stdout.lock(), png.get_ref(), cols, rows)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wide_and_tall_images_fit_the_actual_terminal() {
        for (w, h) in [(4000, 1000), (500, 8000), (1, 1)] {
            for geometry in [(38, 20, 14, 30), (160, 50, 16, 32)] {
                let (width, height, cols, rows) = display_size(w, h, geometry);
                assert!(cols <= geometry.0 - 2 && rows <= geometry.1 / 2);
                assert!(width <= w && height <= h);
            }
        }
    }
    #[test]
    fn kitty_chunks_roundtrip_and_reserve_lines() {
        let png: Vec<_> = (0..15000).map(|i| (i % 251) as u8).collect();
        let mut bytes = Vec::new();
        emit_png(&mut bytes, &png, 40, 10).unwrap();
        let wire = String::from_utf8(bytes).unwrap();
        assert!(wire.starts_with(&format!("{}\x1b[10A", "\r\n".repeat(11))));
        let mut payload = String::new();
        let frames: Vec<_> = wire.split("\x1b_G").skip(1).collect();
        for (i, frame) in frames.iter().enumerate() {
            let (keys, content) = frame.split_once(';').unwrap();
            let content = content.split("\x1b\\").next().unwrap();
            assert!(content.len() <= 4096 && content.len() % 4 == 0);
            assert!(keys.contains(if i + 1 < frames.len() { "m=1" } else { "m=0" }));
            payload.push_str(content);
        }
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(payload)
                .unwrap(),
            png
        );
        assert!(wire.ends_with("\x1b[10B\r\n"));
    }
}
