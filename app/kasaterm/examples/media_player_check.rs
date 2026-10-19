//! 네이티브 재생기 실기 검사. 메인 스레드에서 실제 AVPlayer 를 돌려 프레임 진행·위치 찾기·
//! 끝/다시 재생·오디오 시계·회전·해제를 확인한다. 앱을 띄우지 않는다.
//!
//!   cargo run -p kasaterm --example media_player_check
//!   cargo test -p kasaterm --example media_player_check   # 순수 함수 단위 시험
//!   cargo run -p kasaterm --example media_player_check -- probe FILE...   # 형식 지원 확인
//!
//! 픽스처는 모두 무음이고 음량도 0 으로 둔다. 다시 만들 때(ffmpeg 9):
//!   colors_av.mp4      64x48, 0–1s 빨강·1–2s 초록·2–3s 파랑 + 무음 AAC
//!   quadrants.mp4      64x32, 왼위 빨강·오위 초록·왼아래 파랑·오아래 흰색
//!   quadrants_rot90.mp4  위 파일에 `-display_rotation:v:0 90 -c copy`
//!   silence.m4a        3s 무음 AAC

#[cfg(target_os = "macos")]
#[path = "../src/media_player.rs"]
#[allow(dead_code)]
mod media_player;

#[cfg(target_os = "macos")]
type Check = fn() -> Result<(), String>;

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("probe") {
        for file in &args[1..] {
            match media_player::probe(std::path::Path::new(file)) {
                Ok(p) => println!(
                    "{file}: audio={} video={} {:?}ms {:?}x{:?} poster={:?}B",
                    p.has_audio,
                    p.has_video,
                    p.duration_ms,
                    p.width,
                    p.height,
                    p.poster.as_ref().map(Vec::len)
                ),
                Err(e) => println!("{file}: error {e}"),
            }
        }
        return;
    }
    let checks: &[(&str, Check)] = &[
        ("probe", mac::probe_files),
        (
            "paused open shows first frame",
            mac::opens_paused_with_frame,
        ),
        (
            "play advances frames on the audio clock",
            mac::play_advances,
        ),
        (
            "pause holds, seek lands on the right frame",
            mac::pause_and_seek,
        ),
        ("end then replay from start", mac::end_and_replay),
        ("audio-only clock", mac::audio_only_clock),
        ("orientation matches AVFoundation poster", mac::orientation),
        ("drop stops and releases", mac::drop_releases),
        ("open errors", mac::open_errors),
    ];
    let mut failed = 0;
    for (name, check) in checks {
        match check() {
            Ok(()) => println!("ok   {name}"),
            Err(e) => {
                failed += 1;
                println!("FAIL {name}: {e}");
            }
        }
    }
    if failed > 0 {
        eprintln!("{failed} check(s) failed");
        std::process::exit(1);
    }
    println!("all {} checks passed", checks.len());
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("media_player_check: macOS only");
}

#[cfg(target_os = "macos")]
mod mac {
    use super::media_player::{probe, Frame, Player};
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: u8) -> i32;
    }

    /// 앱에선 winit 이 메인 런루프를 돌린다. 여기선 직접 돌려야 AVPlayer 상태가 갱신된다.
    fn pump(seconds: f64) {
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 0) };
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/media_player/fixtures")
            .join(name)
    }

    fn open(name: &str) -> Result<Player, String> {
        let mut p = Player::open(&fixture(name))?;
        p.set_volume(0.0);
        Ok(p)
    }

    fn wait<T>(timeout: f64, mut f: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        while Instant::now() < deadline {
            if let Some(v) = f() {
                return Some(v);
            }
            pump(0.005);
        }
        None
    }

    fn wait_ready(p: &Player) -> Result<f64, String> {
        wait(5.0, || {
            let s = p.state();
            if let Some(e) = s.error {
                return Some(Err(e));
            }
            s.duration.map(Ok)
        })
        .unwrap_or_else(|| Err("player never became ready".into()))
    }

    fn next_frame(p: &mut Player, timeout: f64) -> Result<Frame, String> {
        wait(timeout, || p.take_frame()).ok_or_else(|| "no frame".to_string())
    }

    fn pixel(f: &Frame, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * f.width + x) * 4) as usize;
        [f.rgba[i], f.rgba[i + 1], f.rgba[i + 2]]
    }

    #[derive(Debug, PartialEq, Clone, Copy)]
    enum Hue {
        Red,
        Green,
        Blue,
        White,
        Other,
    }

    fn hue([r, g, b]: [u8; 3]) -> Hue {
        let hi = |v: u8| v > 180;
        let lo = |v: u8| v < 80;
        match (hi(r), hi(g), hi(b)) {
            (true, true, true) => Hue::White,
            (true, false, false) if lo(g) && lo(b) => Hue::Red,
            (false, true, false) if lo(r) && lo(b) => Hue::Green,
            (false, false, true) if lo(r) && lo(g) => Hue::Blue,
            _ => Hue::Other,
        }
    }

    fn center(f: &Frame) -> Hue {
        hue(pixel(f, f.width / 2, f.height / 2))
    }

    /// 네 사분면 가운데 색: 왼위·오위·왼아래·오아래.
    fn quadrants(f: &Frame) -> [Hue; 4] {
        let (qx, qy) = (f.width / 4, f.height / 4);
        [
            hue(pixel(f, qx, qy)),
            hue(pixel(f, f.width - 1 - qx, qy)),
            hue(pixel(f, qx, f.height - 1 - qy)),
            hue(pixel(f, f.width - 1 - qx, f.height - 1 - qy)),
        ]
    }

    fn ensure(cond: bool, msg: impl FnOnce() -> String) -> Result<(), String> {
        if cond {
            Ok(())
        } else {
            Err(msg())
        }
    }

    fn png_frame(png: &[u8]) -> Result<Frame, String> {
        let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .map_err(|e| format!("poster png: {e}"))?
            .to_rgba8();
        Ok(Frame {
            width: img.width(),
            height: img.height(),
            rgba: img.into_raw(),
            serial: 0,
        })
    }

    pub fn probe_files() -> Result<(), String> {
        let av = probe(&fixture("colors_av.mp4"))?;
        ensure(av.has_audio && av.has_video, || format!("tracks {av:?}"))?;
        ensure(
            av.duration_ms.is_some_and(|d| d.abs_diff(3000) <= 50),
            || format!("duration {:?}", av.duration_ms),
        )?;
        ensure((av.width, av.height) == (Some(64), Some(48)), || {
            format!("size {:?}x{:?}", av.width, av.height)
        })?;
        let poster = png_frame(av.poster.as_deref().ok_or("video poster missing")?)?;
        ensure((poster.width, poster.height) == (64, 48), || {
            format!("poster {}x{}", poster.width, poster.height)
        })?;
        ensure(center(&poster) == Hue::Red, || {
            format!("poster colour {:?}", pixel(&poster, 32, 24))
        })?;

        let audio = probe(&fixture("silence.m4a"))?;
        ensure(audio.has_audio && !audio.has_video, || {
            format!("tracks {audio:?}")
        })?;
        ensure(
            audio.duration_ms.is_some_and(|d| d.abs_diff(3000) <= 50),
            || format!("audio duration {:?}", audio.duration_ms),
        )?;
        ensure(audio.width.is_none() && audio.poster.is_none(), || {
            "audio has no size/poster".into()
        })?;

        let rot = probe(&fixture("quadrants_rot90.mp4"))?;
        ensure((rot.width, rot.height) == (Some(32), Some(64)), || {
            format!("rotated size {:?}x{:?}", rot.width, rot.height)
        })?;

        ensure(probe(&fixture("missing.mp4")).is_err(), || {
            "missing file probed".into()
        })?;
        let text =
            std::env::temp_dir().join(format!("kasa-media-check-{}.mp4", std::process::id()));
        std::fs::write(&text, b"not a movie at all").map_err(|e| e.to_string())?;
        let bogus = probe(&text);
        let _ = std::fs::remove_file(&text);
        ensure(bogus.is_err(), || format!("text file probed as {bogus:?}"))?;
        // 검사는 GUI 스레드 밖에서 돈다.
        std::thread::spawn(|| probe(&fixture("colors_av.mp4")).map(|_| ()))
            .join()
            .map_err(|_| "probe panicked off the main thread".to_string())?
    }

    pub fn opens_paused_with_frame() -> Result<(), String> {
        let mut p = open("colors_av.mp4")?;
        let s = p.state();
        ensure(!s.playing && s.position == 0.0, || {
            format!("fresh state {s:?}")
        })?;
        let duration = wait_ready(&p)?;
        ensure((duration - 3.0).abs() < 0.05, || {
            format!("duration {duration}")
        })?;
        let f = next_frame(&mut p, 3.0)?;
        ensure((f.width, f.height) == (64, 48), || {
            format!("frame {}x{}", f.width, f.height)
        })?;
        ensure(f.rgba.len() == 64 * 48 * 4, || "rgba length".into())?;
        ensure(center(&f) == Hue::Red, || {
            format!("first frame {:?}", pixel(&f, 32, 24))
        })?;
        pump(0.5);
        let s = p.state();
        ensure(!s.playing && s.position < 0.01, || {
            format!("advanced while paused: {s:?}")
        })
    }

    pub fn play_advances() -> Result<(), String> {
        let mut p = open("colors_av.mp4")?;
        wait_ready(&p)?;
        while p.take_frame().is_some() {}
        let clicked = Instant::now();
        p.play();
        wait(2.0, || (p.state().position > 0.0).then_some(())).ok_or("never started")?;
        let latency = clicked.elapsed().as_secs_f64();
        println!("     start latency {:.0} ms", latency * 1000.0);
        ensure(latency < 0.5, || format!("start latency {latency:.3}s"))?;
        let (start, p0) = (Instant::now(), p.state().position);
        let mut serials = Vec::new();
        let mut last = None;
        while start.elapsed() < Duration::from_millis(1100) {
            if let Some(f) = p.take_frame() {
                serials.push(f.serial);
                last = Some(f);
            }
            pump(0.004);
        }
        let s = p.state();
        let wall = start.elapsed().as_secs_f64();
        ensure(s.playing, || format!("not playing {s:?}"))?;
        ensure(((s.position - p0) - wall).abs() < 0.1, || {
            format!("advanced {:.3} in wall {wall:.3}", s.position - p0)
        })?;
        ensure(serials.windows(2).all(|w| w[1] == w[0] + 1), || {
            "serial gaps".into()
        })?;
        ensure(serials.len() >= 25, || {
            format!("only {} frames in 1.1s", serials.len())
        })?;
        let last = last.ok_or("no frames while playing")?;
        ensure(center(&last) == Hue::Green, || {
            format!("frame at {:.2}s is {:?}", s.position, pixel(&last, 32, 24))
        })
    }

    pub fn pause_and_seek() -> Result<(), String> {
        let mut p = open("colors_av.mp4")?;
        wait_ready(&p)?;
        p.play();
        pump(0.4);
        p.pause();
        pump(0.1);
        let held = p.state().position;
        pump(0.4);
        let s = p.state();
        ensure(!s.playing && (s.position - held).abs() < 0.01, || {
            format!("moved while paused {held:.3} -> {:.3}", s.position)
        })?;
        while p.take_frame().is_some() {}
        p.seek(2.5);
        let f = wait(3.0, || p.take_frame().filter(|f| center(f) == Hue::Blue))
            .ok_or("seek to 2.5s never showed the blue frame")?;
        let s = p.state();
        ensure((s.position - 2.5).abs() < 0.05 && !s.playing, || {
            format!("after seek {s:?}")
        })?;
        p.seek(0.5);
        let f2 = wait(3.0, || p.take_frame().filter(|f| center(f) == Hue::Red))
            .ok_or("seek back to 0.5s never showed red")?;
        ensure(f2.serial > f.serial, || "serial did not grow".into())?;
        p.seek(-4.0);
        pump(0.2);
        ensure(p.state().position.abs() < 0.05, || {
            "negative seek not clamped".into()
        })?;
        p.seek(99.0);
        pump(0.2);
        let s = p.state();
        ensure(s.position <= 3.0 + 1e-6 && s.ended, || {
            format!("seek past end {s:?}")
        })
    }

    pub fn end_and_replay() -> Result<(), String> {
        let mut p = open("colors_av.mp4")?;
        wait_ready(&p)?;
        p.seek(2.4);
        pump(0.2);
        p.play();
        wait(3.0, || p.state().ended.then_some(())).ok_or("never ended")?;
        let s = p.state();
        ensure(!s.playing && (s.position - 3.0).abs() < 0.06, || {
            format!("end state {s:?}")
        })?;
        while p.take_frame().is_some() {}
        p.play();
        let f = next_frame(&mut p, 2.0)?;
        pump(0.3);
        let s = p.state();
        ensure(s.playing && !s.ended && s.position < 1.0, || {
            format!("replay state {s:?}")
        })?;
        ensure(center(&f) == Hue::Red, || {
            format!("replay frame {:?}", pixel(&f, 32, 24))
        })
    }

    pub fn audio_only_clock() -> Result<(), String> {
        let mut p = open("silence.m4a")?;
        let duration = wait_ready(&p)?;
        ensure((duration - 3.0).abs() < 0.05, || {
            format!("duration {duration}")
        })?;
        p.play();
        wait(2.0, || (p.state().position > 0.05).then_some(())).ok_or("audio never started")?;
        let (t0, p0) = (Instant::now(), p.state().position);
        let mut frames = 0;
        while t0.elapsed() < Duration::from_millis(1500) {
            frames += p.take_frame().is_some() as u32;
            pump(0.01);
        }
        let rate = (p.state().position - p0) / t0.elapsed().as_secs_f64();
        ensure((0.9..1.1).contains(&rate), || {
            format!("clock rate {rate:.3}")
        })?;
        ensure(frames == 0, || format!("{frames} frames from audio"))
    }

    pub fn orientation() -> Result<(), String> {
        use Hue::*;
        let mut straight = open("quadrants.mp4")?;
        wait_ready(&straight)?;
        let f = next_frame(&mut straight, 3.0)?;
        ensure((f.width, f.height) == (64, 32), || {
            format!("straight {}x{}", f.width, f.height)
        })?;
        ensure(quadrants(&f) == [Red, Green, Blue, White], || {
            format!("straight {:?}", quadrants(&f))
        })?;

        let poster = probe(&fixture("quadrants_rot90.mp4"))?
            .poster
            .ok_or("rotated poster missing")?;
        let reference = png_frame(&poster)?;
        let mut rot = open("quadrants_rot90.mp4")?;
        wait_ready(&rot)?;
        let f = next_frame(&mut rot, 3.0)?;
        ensure(
            (f.width, f.height) == (reference.width, reference.height),
            || {
                format!(
                    "rotated {}x{} vs poster {}x{}",
                    f.width, f.height, reference.width, reference.height
                )
            },
        )?;
        ensure((f.width, f.height) == (32, 64), || {
            format!("rotated {}x{}", f.width, f.height)
        })?;
        let ours = quadrants(&f);
        ensure(
            ours == quadrants(&reference) && !ours.contains(&Other),
            || {
                format!(
                    "rotated {ours:?} vs AVFoundation {:?}",
                    quadrants(&reference)
                )
            },
        )?;
        // ffmpeg 의 90 은 반시계 회전: 오른쪽 위(초록)가 왼쪽 위로 온다.
        ensure(ours == [Green, White, Red, Blue], || {
            format!("rotated {ours:?}")
        })
    }

    pub fn drop_releases() -> Result<(), String> {
        // 앱에선 winit 이벤트 한 바퀴마다 autorelease 풀이 비워진다. 여기선 직접 감싼다.
        let watch = objc2::rc::autoreleasepool(|_| -> Result<_, String> {
            let mut p = open("colors_av.mp4")?;
            wait_ready(&p)?;
            p.play();
            wait(0.3, || p.take_frame().map(|_| ())).ok_or("no frame while playing")?;
            let watch = p.drop_watch();
            ensure(!watch.released(), || "released while alive".into())?;
            drop(p);
            Ok(watch)
        })?;
        wait(3.0, || watch.released().then_some(())).ok_or("AVPlayer still alive after drop")?;
        Ok(())
    }

    pub fn open_errors() -> Result<(), String> {
        ensure(Player::open(&fixture("missing.mp4")).is_err(), || {
            "missing opened".into()
        })?;
        ensure(Player::open(&fixture("")).is_err(), || {
            "directory opened".into()
        })?;
        let text = std::env::temp_dir().join(format!("kasa-media-open-{}.mov", std::process::id()));
        std::fs::write(&text, b"garbage").map_err(|e| e.to_string())?;
        let result = Player::open(&text).and_then(|p| {
            wait(5.0, || p.state().error)
                .ok_or_else(|| "garbage never reported an error".to_string())
        });
        let _ = std::fs::remove_file(&text);
        let message = result?;
        ensure(!message.is_empty(), || "empty error".into())?;
        std::thread::spawn(|| Player::open(&fixture("colors_av.mp4")).err())
            .join()
            .map_err(|_| "open panicked off main".to_string())?
            .map(|_| ())
            .ok_or_else(|| "opened off the main thread".to_string())
    }
}
