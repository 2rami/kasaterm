//! 네이티브 재생기 실기 검사. 메인 스레드에서 실제 AVPlayer 를 돌려 프레임 진행·위치 찾기·
//! 끝/다시 재생·오디오 시계·회전·해제를 확인한다. 앱을 띄우지 않는다. `stream:` 검사는 느린
//! 루프백 HTTP Range 원본으로 `open_url` 을 열어, 파일을 다 받기 전에 그림·소리가 나오고 끝쪽
//! 탐색이 그 구간을 따로 요청하는지 잰다.
//!
//!   cargo run -p kasaterm --example media_player_check
//!   cargo test -p kasaterm --example media_player_check   # 순수 함수 단위 시험
//!   cargo run -p kasaterm --example media_player_check -- probe FILE...   # 형식 지원 확인
//!   cargo run -p kasaterm --example media_player_check -- seek-latency FILE...   # 멈춘 뒤 +5s 이동 → 새 프레임까지
//!   cargo run -p kasaterm --example media_player_check -- stream-trace FILE KIB_PER_S [play|SEEK_S] [AT_BYTES:SECS]
//!       # 느린 원본 타임라인. 마지막 인자는 그만큼 보낸 뒤 원본이 멈추는 시간
//!   cargo run -p kasaterm --example media_player_check -- stream-buffer FILE KIB_PER_S
//!       # 재생·멈춤·먼 탐색 동안 원본에서 나간 양. 점진 받기는 멈춘 동안에도 회선 속도로 앞을 읽고,
//!       # `preferredForwardBufferDuration`(5·15·30초)으로도 그 양이 줄지 않았다(2026-10-21 실측)
//!
//! 픽스처는 무음이거나 아주 작은 소리이고 음량도 0 으로 둔다. 다시 만들 때(ffmpeg 9):
//!   colors_av.mp4      64x48, 0–1s 빨강·1–2s 초록·2–3s 파랑 + 무음 AAC
//!   quadrants.mp4      64x32, 왼위 빨강·오위 초록·왼아래 파랑·오아래 흰색
//!   quadrants_rot90.mp4  위 파일에 `-display_rotation:v:0 90 -c copy`
//!   silence.m4a        3s 무음 AAC
//!   long_gop.mp4       160x90 24fps, 0–4s 빨강·4–8s 파랑(시간 잡음), 키프레임은 0초 하나(`-g 1000 -bf 0`) + 무음 AAC
//!   stream_tail.mp4    320x180 24fps 12s, 4초씩 0xff0000·0x00ff00·0x0000ff(시간 잡음 `noise=alls=24:allf=t`),
//!                      150kbps, 키프레임 2초(`keyint=48:scenecut=0 -bf 0`) + 440Hz `volume=0.03` AAC 32k,
//!                      moov 가 끝(기본 muxer)
//!   stream_tail.m4a    같은 소리 30s, 22050Hz 모노 AAC 24k, moov 가 끝

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
    if args.first().map(String::as_str) == Some("seek-latency") {
        for file in &args[1..] {
            if let Err(e) = mac::seek_latency(std::path::Path::new(file)) {
                println!("{file}: error {e}");
            }
        }
        return;
    }
    if args.first().map(String::as_str) == Some("stream-buffer") {
        if let Err(e) = mac::stream_buffer(&args[1..]) {
            println!("error {e}");
        }
        return;
    }
    if args.first().map(String::as_str) == Some("stream-trace") {
        if let Err(e) = mac::stream_trace(&args[1..]) {
            println!("error {e}");
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
        (
            "paused seek stays seeking until the new frame",
            mac::paused_seek_until_frame,
        ),
        ("audio seek settles without frames", mac::audio_seek_settles),
        ("end then replay from start", mac::end_and_replay),
        ("audio-only clock", mac::audio_only_clock),
        ("orientation matches AVFoundation poster", mac::orientation),
        ("drop stops and releases", mac::drop_releases),
        ("open errors", mac::open_errors),
        (
            "stream: paused first frame before the whole file",
            mac::stream_first_frame,
        ),
        (
            "stream: play starts before the whole file",
            mac::stream_plays_early,
        ),
        (
            "stream: paused seek near the end asks for that range",
            mac::stream_seek_tail,
        ),
        ("stream: seek before ready", mac::stream_seek_before_ready),
        (
            "stream: playback resumes after the source stalls",
            mac::stream_stall_resumes,
        ),
        (
            "stream: audio plays before the whole file",
            mac::stream_audio_early,
        ),
        (
            "stream: drop stops pulling bytes",
            mac::stream_drop_stops_pulling,
        ),
        ("stream: bad URLs and HTTP errors", mac::stream_errors),
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

    /// 앱처럼 `playing || buffering || seeking` 일 때만 프레임을 꺼내는 렌더 루프를 흉내 낸다.
    /// 고정 유예 없이도 멈춘 채 이동한 자리의 새 프레임까지 루프가 이어져야 한다.
    fn drive(p: &mut Player, limit: f64) -> Vec<Frame> {
        let mut frames = Vec::new();
        let deadline = Instant::now() + Duration::from_secs_f64(limit);
        while Instant::now() < deadline {
            let s = p.state();
            if !(s.playing || s.buffering || s.seeking) {
                break;
            }
            frames.extend(p.take_frame());
            pump(0.004);
        }
        frames
    }

    pub fn paused_seek_until_frame() -> Result<(), String> {
        let mut p = open("long_gop.mp4")?;
        ensure(p.state().seeking, || {
            "fresh player not waiting for its first frame".into()
        })?;
        let first = drive(&mut p, 5.0);
        ensure(!p.state().seeking, || "still seeking after open".into())?;
        let first = first
            .last()
            .ok_or("render loop stopped before the first frame")?;
        ensure(center(first) == Hue::Red, || {
            format!("first frame {:?}", pixel(first, 80, 45))
        })?;

        p.play();
        wait(3.0, || (p.state().position > 0.7).then_some(())).ok_or("never played")?;
        p.pause();
        pump(0.1);
        while p.take_frame().is_some() {}
        ensure(!p.state().seeking, || "seeking before any seek".into())?;

        // 앱의 고정 유예(1초)보다 오래 프레임을 안 꺼내도 이동은 끝나지 않은 것으로 남아야 한다.
        p.seek(5.0);
        ensure(p.state().seeking, || "seek did not raise seeking".into())?;
        pump(1.5);
        let s = p.state();
        ensure(s.seeking && (s.position - 5.0).abs() < 0.05, || {
            format!("after 1.5s without frames {s:?}")
        })?;
        let frames = drive(&mut p, 5.0);
        let s = p.state();
        ensure(!s.seeking && !s.playing, || format!("loop ended in {s:?}"))?;
        let last = frames
            .last()
            .ok_or("render loop stopped before the new frame")?;
        ensure(center(last) == Hue::Blue, || {
            format!(
                "last frame before the loop stopped is {:?}",
                pixel(last, 80, 45)
            )
        })?;

        // 막 이동한 뒤 바로 꺼내기 시작해도(옛 프레임이 먼저 올 수 있다) 새 프레임에서야 내려온다.
        p.seek(1.0);
        let frames = drive(&mut p, 5.0);
        let last = frames.last().ok_or("no frame after seeking back")?;
        ensure(center(last) == Hue::Red && !p.state().seeking, || {
            format!(
                "seek back ended on {:?} {:?}",
                pixel(last, 80, 45),
                p.state()
            )
        })?;

        // 끝으로 보내도 루프가 영영 돌지는 않는다.
        p.seek(99.0);
        drive(&mut p, 7.0);
        let s = p.state();
        ensure(!s.seeking && s.ended, || format!("seek to end left {s:?}"))
    }

    pub fn audio_seek_settles() -> Result<(), String> {
        let mut p = open("silence.m4a")?;
        wait_ready(&p)?;
        wait(2.0, || (!p.state().seeking).then_some(())).ok_or("audio open stuck seeking")?;
        p.seek(1.5);
        ensure(p.state().seeking, || {
            "audio seek did not raise seeking".into()
        })?;
        wait(2.0, || (!p.state().seeking).then_some(())).ok_or("audio seek never settled")?;
        let s = p.state();
        ensure((s.position - 1.5).abs() < 0.05, || {
            format!("audio seek {s:?}")
        })?;
        ensure(p.take_frame().is_none(), || "audio produced a frame".into())
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

    /// 0.7초 재생 → 멈춤 → +5초 이동 뒤 위치·새 프레임이 언제 오는지 잰다.
    pub fn seek_latency(path: &std::path::Path) -> Result<(), String> {
        let mut p = Player::open(path)?;
        p.set_volume(0.0);
        wait_ready(&p)?;
        p.play();
        wait(3.0, || (p.state().position > 0.7).then_some(()));
        p.pause();
        pump(0.1);
        while p.take_frame().is_some() {}
        let from = p.state().position;
        let target = from + 5.0;
        let started = Instant::now();
        p.seek(target);
        let mut moved = None;
        let mut frame = None;
        while started.elapsed() < Duration::from_secs(5) && frame.is_none() {
            let s = p.state();
            if moved.is_none() && (s.position - target).abs() < 0.05 {
                moved = Some(started.elapsed());
            }
            if let Some(f) = p.take_frame() {
                frame = Some((started.elapsed(), f.serial));
            }
            pump(0.002);
        }
        println!(
            "{}: paused {from:.3} -> {target:.3}  position after {:?}  frame after {:?}  state {:?}",
            path.display(),
            moved,
            frame,
            p.state()
        );
        Ok(())
    }

    fn fixture_bytes(name: &str) -> Result<Vec<u8>, String> {
        std::fs::read(fixture(name)).map_err(|e| format!("{name}: {e}"))
    }

    /// 최상위 상자 위치. moov 가 끝에 있는 픽스처인지와 mdat 구간을 확인하는 데 쓴다.
    fn top_box(bytes: &[u8], kind: &[u8; 4]) -> Option<(u64, u64)> {
        let mut at = 0usize;
        while at + 8 <= bytes.len() {
            let size = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
            if size < 8 {
                return None;
            }
            if &bytes[at + 4..at + 8] == kind {
                return Some((at as u64, size as u64));
            }
            at += size;
        }
        None
    }

    fn stream_open(server: &range::Server) -> Result<Player, String> {
        let mut p = Player::open_url(&server.url)?;
        p.set_volume(0.0);
        Ok(p)
    }

    /// 앱 렌더 루프처럼 프레임을 꺼내며 조건을 기다린다. 이룬 때 마지막으로 받은 프레임을 돌려준다.
    fn drive_until(
        p: &mut Player,
        limit: f64,
        mut done: impl FnMut(&Player, Option<&Frame>) -> bool,
    ) -> Result<Option<Frame>, String> {
        let deadline = Instant::now() + Duration::from_secs_f64(limit);
        let mut last = None;
        while Instant::now() < deadline {
            if let Some(f) = p.take_frame() {
                last = Some(f);
            }
            if done(p, last.as_ref()) {
                return Ok(last);
            }
            if let Some(e) = p.state().error {
                return Err(e);
            }
            pump(0.004);
        }
        Err(format!("timed out in {:?}", p.state()))
    }

    pub fn stream_first_frame() -> Result<(), String> {
        let bytes = fixture_bytes("stream_tail.mp4")?;
        let (moov, _) = top_box(&bytes, b"moov").ok_or("no moov")?;
        ensure(moov > bytes.len() as u64 / 2, || {
            "fixture moov is not at the end".into()
        })?;
        let server = range::Server::start(bytes, "video/mp4", 64 * 1024, 200);
        let started = Instant::now();
        let mut p = stream_open(&server)?;
        ensure(p.state().buffering && p.state().seeking, || {
            format!("fresh stream not loading {:?}", p.state())
        })?;
        let frame = drive_until(&mut p, 10.0, |_, f| f.is_some())
            .map_err(|e| format!("no first frame: {e}; {}", server.summary()))?
            .ok_or("no first frame")?;
        let sent = server.sent();
        println!(
            "     first frame {:.0} ms after {sent}/{} B; {}",
            started.elapsed().as_secs_f64() * 1000.0,
            server.size,
            server.summary()
        );
        ensure(center(&frame) == Hue::Red, || {
            format!("first frame {:?}", pixel(&frame, 160, 90))
        })?;
        ensure(sent * 10 < server.size * 6, || {
            format!("{sent}/{} B before the first frame", server.size)
        })?;
        ensure(server.requests().iter().any(|r| r.start >= moov), || {
            format!("tail moov never asked by range: {}", server.summary())
        })?;
        let s = p.state();
        ensure(
            !s.playing && s.duration.is_some_and(|d| (d - 12.0).abs() < 0.1),
            || format!("after first frame {s:?}"),
        )
    }

    pub fn stream_plays_early() -> Result<(), String> {
        let server = range::Server::start(
            fixture_bytes("stream_tail.mp4")?,
            "video/mp4",
            64 * 1024,
            200,
        );
        let started = Instant::now();
        let mut p = stream_open(&server)?;
        p.play();
        drive_until(&mut p, 10.0, |p, _| p.state().position > 0.5)
            .map_err(|e| format!("never played: {e}; {}", server.summary()))?;
        let sent = server.sent();
        println!(
            "     0.5 s played {:.0} ms after start, {sent}/{} B",
            started.elapsed().as_secs_f64() * 1000.0,
            server.size
        );
        ensure(sent < server.size, || {
            "whole file arrived before playback".into()
        })?;
        let (t0, p0) = (Instant::now(), p.state().position);
        drive_until(&mut p, 2.0, |_, _| t0.elapsed() > Duration::from_millis(1000))?;
        let rate = (p.state().position - p0) / t0.elapsed().as_secs_f64();
        ensure((0.8..1.2).contains(&rate), || format!("clock rate {rate:.3}"))
    }

    pub fn stream_seek_tail() -> Result<(), String> {
        let bytes = fixture_bytes("stream_tail.mp4")?;
        let (moov, _) = top_box(&bytes, b"moov").ok_or("no moov")?;
        let (mdat, mdat_len) = top_box(&bytes, b"mdat").ok_or("no mdat")?;
        let server = range::Server::start(bytes, "video/mp4", 64 * 1024, 200);
        let mut p = stream_open(&server)?;
        drive_until(&mut p, 10.0, |_, f| f.is_some())?;
        let before = server.requests().len();
        let started = Instant::now();
        p.seek(10.5);
        ensure(p.state().seeking, || "seek did not raise seeking".into())?;
        drive_until(&mut p, 10.0, |p, f| {
            !p.state().seeking && f.is_some_and(|f| center(f) == Hue::Blue)
        })
        .map_err(|e| format!("blue frame never came: {e}; {}", server.summary()))?;
        let sent = server.sent();
        println!(
            "     tail frame {:.0} ms after seek, {sent}/{} B; {}",
            started.elapsed().as_secs_f64() * 1000.0,
            server.size,
            server.summary()
        );
        let s = p.state();
        ensure(!s.playing && (s.position - 10.5).abs() < 0.05, || {
            format!("after seek {s:?}")
        })?;
        let tail = mdat + mdat_len / 2;
        ensure(
            server.requests()[before..]
                .iter()
                .any(|r| r.start >= tail && r.start < moov),
            || format!("no range request into the tail of mdat: {}", server.summary()),
        )?;
        ensure(sent < server.size, || {
            "whole file arrived before the tail frame".into()
        })
    }

    pub fn stream_seek_before_ready() -> Result<(), String> {
        let server = range::Server::start(
            fixture_bytes("stream_tail.mp4")?,
            "video/mp4",
            64 * 1024,
            200,
        );
        let mut p = stream_open(&server)?;
        p.seek(5.0);
        drive_until(&mut p, 10.0, |p, f| {
            !p.state().seeking && f.is_some_and(|f| center(f) == Hue::Green)
        })
        .map_err(|e| format!("green frame never came: {e}; {}", server.summary()))?;
        let s = p.state();
        ensure((s.position - 5.0).abs() < 0.05, || {
            format!("early seek landed {s:?}")
        })
    }

    pub fn stream_stall_resumes() -> Result<(), String> {
        let mut server = range::Server::start(
            fixture_bytes("stream_tail.mp4")?,
            "video/mp4",
            64 * 1024,
            200,
        );
        server.stall_after(120 * 1024, Duration::from_secs(4));
        let mut p = stream_open(&server)?;
        p.play();
        let mut stalled = None;
        drive_until(&mut p, 25.0, |p, _| {
            let s = p.state();
            if s.buffering && s.position > 0.5 && stalled.is_none() {
                stalled = Some(s.position);
            }
            stalled.is_some() && s.position > 9.0
        })
        .map_err(|e| {
            format!(
                "did not resume: {e}; stalled at {stalled:?}; {}",
                server.summary()
            )
        })?;
        println!(
            "     stalled at {:.2}s, resumed to {:.2}s",
            stalled.unwrap_or(0.0),
            p.state().position
        );
        ensure(p.state().playing, || {
            format!("not playing after resume {:?}", p.state())
        })
    }

    pub fn stream_audio_early() -> Result<(), String> {
        let bytes = fixture_bytes("stream_tail.m4a")?;
        let (moov, _) = top_box(&bytes, b"moov").ok_or("no moov")?;
        ensure(moov > bytes.len() as u64 / 2, || {
            "fixture moov is not at the end".into()
        })?;
        let server = range::Server::start(bytes, "audio/mp4", 16 * 1024, 200);
        let started = Instant::now();
        let mut p = stream_open(&server)?;
        p.play();
        drive_until(&mut p, 10.0, |p, _| p.state().position > 0.3)
            .map_err(|e| format!("audio never played: {e}; {}", server.summary()))?;
        let sent = server.sent();
        println!(
            "     audio 0.3 s played {:.0} ms after start, {sent}/{} B",
            started.elapsed().as_secs_f64() * 1000.0,
            server.size
        );
        ensure(sent < server.size, || "whole file arrived before audio".into())?;
        ensure(server.requests().iter().any(|r| r.start >= moov), || {
            format!("tail moov never asked by range: {}", server.summary())
        })?;
        ensure(p.take_frame().is_none(), || "audio produced a frame".into())
    }

    pub fn stream_drop_stops_pulling() -> Result<(), String> {
        let server = range::Server::start(
            fixture_bytes("stream_tail.mp4")?,
            "video/mp4",
            24 * 1024,
            200,
        );
        let watch = objc2::rc::autoreleasepool(|_| -> Result<_, String> {
            let mut p = stream_open(&server)?;
            p.play();
            drive_until(&mut p, 10.0, |_, f| f.is_some())?;
            ensure(server.busy() > 0, || "nothing in flight to cancel".into())?;
            let watch = p.drop_watch();
            drop(p);
            Ok(watch)
        })?;
        wait(3.0, || (server.busy() == 0).then_some(()))
            .ok_or_else(|| format!("still sending after drop: {}", server.summary()))?;
        let sent = server.sent();
        pump(1.0);
        ensure(server.sent() == sent, || "bytes kept flowing after drop".into())?;
        ensure(sent < server.size, || "whole file was pulled".into())?;
        wait(3.0, || watch.released().then_some(())).ok_or("AVPlayer still alive after drop")?;
        Ok(())
    }

    /// 느린 원본에서 열고(재생 또는 탐색) 0.25초마다 받은 양·요청·상태를 찍는다.
    pub fn stream_trace(args: &[String]) -> Result<(), String> {
        let [file, rate, rest @ ..] = args else {
            return Err("usage: stream-trace FILE KIB_PER_S [play|SEEK_S] [AT_BYTES:SECS]".into());
        };
        let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
        let mime = if file.ends_with(".m4a") { "audio/mp4" } else { "video/mp4" };
        let rate: usize = rate.parse().map_err(|_| "bad rate")?;
        let mut server = range::Server::start(bytes, mime, rate * 1024, 200);
        if let Some((at, secs)) = rest.get(1).and_then(|h| h.split_once(':')) {
            let at = at.parse().map_err(|_| "bad stall offset")?;
            let secs: f64 = secs.parse().map_err(|_| "bad stall seconds")?;
            server.stall_after(at, Duration::from_secs_f64(secs));
        }
        let started = Instant::now();
        let mut p = stream_open(&server)?;
        match rest.first().map(String::as_str) {
            Some("play") => p.play(),
            Some(s) => p.seek(s.parse().map_err(|_| "bad seek")?),
            None => {}
        }
        let mut frames = 0;
        let mut next = Duration::ZERO;
        while started.elapsed() < Duration::from_secs(12) {
            if let Some(f) = p.take_frame() {
                frames += 1;
                println!("{:6.0} ms frame #{frames} {:?} at {:.2}s", started.elapsed().as_secs_f64() * 1000.0, center(&f), p.state().position);
            }
            if started.elapsed() >= next {
                next += Duration::from_millis(250);
                let s = p.state();
                println!(
                    "{:6.0} ms pos {:.2} dur {:?} play {} buf {} seek {} err {:?} | {}",
                    started.elapsed().as_secs_f64() * 1000.0,
                    s.position, s.duration, s.playing, s.buffering, s.seeking, s.error, server.summary()
                );
            }
            pump(0.004);
        }
        Ok(())
    }

    /// 재생 6초 → 멈춤 6초 → 90초로 이동 → 재생 4초 → 닫기 동안 원본에서 나간 양을 잰다.
    pub fn stream_buffer(args: &[String]) -> Result<(), String> {
        let [file, rate] = args else {
            return Err("usage: stream-buffer FILE KIB_PER_S".into());
        };
        let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
        let rate: usize = rate.parse().map_err(|_| "bad rate")?;
        let server = range::Server::start(bytes, "video/mp4", rate * 1024, 200);
        let mut p = stream_open(&server)?;
        p.play();
        let mark = |label: &str, p: &mut Player, secs: f64| {
            let until = Instant::now() + Duration::from_secs_f64(secs);
            while Instant::now() < until {
                p.take_frame();
                pump(0.004);
            }
            println!(
                "{label:>12}: pos {:6.2} sent {:>9} requests {}",
                p.state().position,
                server.sent(),
                server.requests().len()
            );
        };
        mark("played 6s", &mut p, 6.0);
        p.pause();
        mark("paused 6s", &mut p, 6.0);
        p.seek(90.0);
        p.play();
        mark("at 90 +4s", &mut p, 4.0);
        drop(p);
        pump(1.0);
        println!("{:>12}: sent {} of {}", "closed", server.sent(), server.size);
        Ok(())
    }

    pub fn stream_errors() -> Result<(), String> {
        for bad in ["https://example.com/a.mp4", "http://localhost:80/a", "file:///tmp/a.mp4"] {
            ensure(Player::open_url(bad).is_err(), || format!("{bad} opened"))?;
        }
        let server = range::Server::start(fixture_bytes("stream_tail.mp4")?, "video/mp4", 64 * 1024, 404);
        let p = stream_open(&server)?;
        let error = wait(10.0, || p.state().error).ok_or("404 never reported an error")?;
        ensure(!p.state().buffering && !p.state().seeking, || {
            format!("failed stream still loading {:?}", p.state())
        })?;
        ensure(!error.is_empty(), || "empty error".into())
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

    /// AVFoundation 이 읽을 느린 HTTP Range 원본. 연결이 여럿이어도 한 줄 회선처럼 대역을 나눈다.
    mod range {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        #[derive(Clone, Debug)]
        pub struct Request {
            pub head: bool,
            pub start: u64,
            pub end: u64,
        }

        struct Shared {
            bytes: Vec<u8>,
            mime: &'static str,
            status: u16,
            rate: f64,
            line: Mutex<Instant>,
            requests: Mutex<Vec<Request>>,
            sent: AtomicU64,
            busy: AtomicUsize,
            stop: AtomicBool,
            stall: Mutex<Option<Arc<Stall>>>,
        }

        pub struct Server {
            pub url: String,
            pub size: u64,
            shared: Arc<Shared>,
        }

        /// 원본 기기가 잠깐 멈춘 것처럼 — 모든 연결을 합쳐 `at` 바이트를 보낸 뒤 한 번 쉰다.
        struct Stall {
            at: u64,
            pause: Duration,
            until: Mutex<Option<Instant>>,
        }

        impl Server {
            pub fn start(bytes: Vec<u8>, mime: &'static str, rate: usize, status: u16) -> Self {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                listener.set_nonblocking(true).unwrap();
                // 앱 스트림처럼 확장자 없는 추측 불가 경로 — 형식은 Content-Type 만으로 알린다.
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let url = format!("http://{}/m/{nonce:032x}", listener.local_addr().unwrap());
                let shared = Arc::new(Shared {
                    bytes,
                    mime,
                    status,
                    rate: rate as f64,
                    line: Mutex::new(Instant::now()),
                    requests: Mutex::default(),
                    sent: AtomicU64::new(0),
                    busy: AtomicUsize::new(0),
                    stop: AtomicBool::new(false),
                    stall: Mutex::default(),
                });
                let size = shared.bytes.len() as u64;
                let s = shared.clone();
                std::thread::spawn(move || {
                    while !s.stop.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                let s = s.clone();
                                std::thread::spawn(move || connection(stream, &s));
                            }
                            Err(_) => std::thread::sleep(Duration::from_millis(5)),
                        }
                    }
                });
                Self { url, size, shared }
            }

            pub fn stall_after(&mut self, at: u64, pause: Duration) {
                *self.shared.stall.lock().unwrap() = Some(Arc::new(Stall {
                    at,
                    pause,
                    until: Mutex::default(),
                }));
            }

            pub fn sent(&self) -> u64 {
                self.shared.sent.load(Ordering::Relaxed)
            }

            pub fn busy(&self) -> usize {
                self.shared.busy.load(Ordering::Relaxed)
            }

            pub fn requests(&self) -> Vec<Request> {
                self.shared.requests.lock().unwrap().clone()
            }

            pub fn summary(&self) -> String {
                let ranges: Vec<String> = self
                    .requests()
                    .iter()
                    .map(|r| format!("{}{}-{}", if r.head { "HEAD " } else { "" }, r.start, r.end))
                    .collect();
                format!("sent {}/{} ranges [{}]", self.sent(), self.size, ranges.join(", "))
            }
        }

        impl Drop for Server {
            fn drop(&mut self) {
                self.shared.stop.store(true, Ordering::Relaxed);
            }
        }

        /// `bytes=a-b` · `bytes=a-` · `bytes=-n` 하나만. 범위 밖이면 None(416).
        fn parse_range(value: &str, size: u64) -> Option<(u64, u64)> {
            let spec = value.trim().strip_prefix("bytes=")?;
            let (a, b) = spec.split_once('-')?;
            let (start, end) = if a.is_empty() {
                let n: u64 = b.parse().ok()?;
                (size.checked_sub(n.min(size))?, size.checked_sub(1)?)
            } else {
                let start: u64 = a.parse().ok()?;
                let end = if b.is_empty() { size - 1 } else { b.parse::<u64>().ok()?.min(size - 1) };
                (start, end)
            };
            (start <= end && end < size).then_some((start, end))
        }

        fn connection(mut stream: TcpStream, s: &Shared) {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let mut buf = Vec::new();
            loop {
                let end = loop {
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                    if s.stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let mut chunk = [0u8; 4096];
                    match stream.read(&mut chunk) {
                        Ok(0) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                        Err(_) => return,
                    }
                };
                let text = String::from_utf8_lossy(&buf[..end]).into_owned();
                buf.drain(..end);
                let head = text.starts_with("HEAD ");
                let size = s.bytes.len() as u64;
                if s.status != 200 {
                    let _ = write!(stream, "HTTP/1.1 {} Not Found\r\nContent-Length: 0\r\n\r\n", s.status);
                    continue;
                }
                let range = text.split("\r\n").find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("range").then(|| v.to_string())
                });
                let (status, start, last) = match &range {
                    None => ("200 OK", 0, size - 1),
                    Some(v) => match parse_range(v, size) {
                        Some((a, b)) => ("206 Partial Content", a, b),
                        None => {
                            let _ = write!(stream, "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\nContent-Length: 0\r\n\r\n");
                            continue;
                        }
                    },
                };
                s.requests.lock().unwrap().push(Request { head, start, end: last });
                let content_range = if range.is_some() {
                    format!("Content-Range: bytes {start}-{last}/{size}\r\n")
                } else {
                    String::new()
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {}\r\nAccept-Ranges: bytes\r\n{content_range}Content-Length: {}\r\nCache-Control: no-store\r\n\r\n",
                    s.mime,
                    last - start + 1
                );
                if stream.write_all(header.as_bytes()).is_err() {
                    return;
                }
                if head {
                    continue;
                }
                s.busy.fetch_add(1, Ordering::Relaxed);
                let ok = body(&mut stream, s, start, last);
                s.busy.fetch_sub(1, Ordering::Relaxed);
                if !ok {
                    return;
                }
            }
        }

        fn body(stream: &mut TcpStream, s: &Shared, start: u64, last: u64) -> bool {
            const PIECE: u64 = 2048;
            let mut at = start;
            while at <= last {
                if s.stop.load(Ordering::Relaxed) {
                    return false;
                }
                let n = PIECE.min(last + 1 - at);
                let stall = s.stall.lock().unwrap().clone();
                if let Some(stall) = stall.filter(|st| s.sent.load(Ordering::Relaxed) >= st.at) {
                    let until = *stall
                        .until
                        .lock()
                        .unwrap()
                        .get_or_insert_with(|| Instant::now() + stall.pause);
                    while Instant::now() < until && !s.stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                let slot = {
                    let mut line = s.line.lock().unwrap();
                    let slot = (*line).max(Instant::now());
                    *line = slot + Duration::from_secs_f64(n as f64 / s.rate);
                    slot
                };
                if let Some(wait) = slot.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                if stream.write_all(&s.bytes[at as usize..(at + n) as usize]).is_err() {
                    return false;
                }
                s.sent.fetch_add(n, Ordering::Relaxed);
                at += n;
            }
            true
        }

        #[test]
        fn ranges_follow_rfc_forms() {
            assert_eq!(parse_range("bytes=0-1", 10), Some((0, 1)));
            assert_eq!(parse_range("bytes=4-", 10), Some((4, 9)));
            assert_eq!(parse_range("bytes=-3", 10), Some((7, 9)));
            assert_eq!(parse_range("bytes=2-99", 10), Some((2, 9)));
            assert_eq!(parse_range("bytes=10-", 10), None);
            assert_eq!(parse_range("bytes=5-2", 10), None);
            assert_eq!(parse_range("items=0-1", 10), None);
        }
    }
}
