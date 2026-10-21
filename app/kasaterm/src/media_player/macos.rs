use std::cell::Cell;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use objc2::rc::{autoreleasepool, Retained, Weak};
use objc2::encode::{Encode, Encoding};
use objc2::runtime::{AnyObject, Bool};
use objc2::{msg_send, AnyThread, MainThreadMarker};
use objc2_av_foundation::{
    AVAsset, AVAssetImageGenerator, AVAssetTrack, AVAsynchronousKeyValueLoading, AVKeyValueStatus,
    AVMediaTypeAudio, AVMediaTypeVideo, AVPlayer, AVPlayerActionAtItemEnd, AVPlayerItem,
    AVPlayerItemStatus, AVPlayerItemVideoOutput, AVPlayerStatus, AVPlayerTimeControlStatus,
    AVURLAsset, AVURLAssetPreferPreciseDurationAndTimingKey,
};
use objc2_core_foundation::{CFString, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
};
use objc2_core_media::{kCMTimeZero, CMTime, CMTimeFlags};
use objc2_core_video::{
    kCVPixelBufferHeightKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_32BGRA, kCVReturnSuccess, CVPixelBuffer, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString, NSURL, NSValue};

use super::{
    playback_ended, Frame, Orientation, Playback, Probe, POSTER_MAX_BYTES, POSTER_MAX_EDGE,
};

/// 프레임을 매번 CPU 로 RGBA 변환하므로 4K 원본은 이 긴 변으로 줄여 받는다(VideoToolbox 가 줄인다).
const FRAME_MAX_EDGE: f64 = 1920.0;
/// 정확한 위치 찾기에 쓰는 시간 단위. 600 은 24·25·30·60fps 를 모두 나눠 떨어뜨린다.
const SEEK_TIMESCALE: i32 = 600;
/// 깨진 파일·느린 볼륨에서 검사 스레드가 영영 붙잡히지 않게.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// 끝 지점처럼 새 프레임이 끝내 안 나오는 위치 찾기가 렌더 루프를 영영 붙잡지 않게. 스트림은
/// 위치 찾기 자체가 받기를 기다리므로 요청 시각이 아니라 완료를 본 때부터 센다.
const SEEK_GIVE_UP: Duration = Duration::from_secs(5);
/// 재생을 기다리는 중 지금 위치 앞으로 이만큼 받혀 있으면 바로 튼다(`start_when_buffered`).
const START_AHEAD: f64 = 3.0;
/// 소리만 있는 항목은 받힌 구간을 알 수 없어(`start_when_buffered`) 이만큼 기다린 뒤 튼다.
const AUDIO_START_WAIT: Duration = Duration::from_secs(1);
/// 정확한 위치 찾기라 새 위치의 프레임은 목표보다 한 프레임 안쪽 앞이다. 24fps 두 장 남짓.
const LANDED_SLACK: f64 = 0.1;

extern "C" {
    fn mach_absolute_time() -> u64;
}

pub struct Player {
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    output: Retained<AVPlayerItemVideoOutput>,
    /// 항목이 준비된 뒤 한 번 정한다. 그 전엔 프레임을 내지 않는다.
    orientation: Option<Orientation>,
    has_video: Option<bool>,
    serial: u64,
    /// 열기도 0초로 가는 위치 찾기로 친다 — 준비된 뒤 첫 프레임이 나올 때까지 `seeking`.
    pending: Option<PendingSeek>,
    seek_generation: u64,
    /// 완료 콜백이 끝난 위치 찾기의 세대를 올린다. 콜백 큐가 문서로 정해져 있지 않아 원자값으로 둔다.
    seek_done: Arc<AtomicU64>,
    /// 재생을 요청했는데 AVPlayer 가 받기를 기다리기 시작한 때.
    waiting_since: Option<Instant>,
}

/// 멈춘 채 위치를 옮기면 새 프레임은 완료 콜백 뒤에야 나오고, 그 전엔 옛 위치의 프레임이 한 장
/// 더 나올 수 있다. 그 사이 렌더 루프가 쉬면 막대만 옮겨 가고 그림은 옛 것에 멈춘다.
struct PendingSeek {
    generation: u64,
    target: f64,
    settled_at: Cell<Option<Instant>>,
}

impl PendingSeek {
    fn new(generation: u64, target: f64) -> Self {
        Self {
            generation,
            target,
            settled_at: Cell::new(None),
        }
    }
}

impl Player {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mtm = main_thread()?;
        let path = local_file(path)?;
        let url = file_url(&path)?;
        Ok(Self::with_url(&url, mtm))
    }

    pub fn open_url(url: &str) -> Result<Self, String> {
        let mtm = main_thread()?;
        let url = NSURL::URLWithString(&NSString::from_str(url))
            .ok_or_else(|| "media stream URL is malformed".to_string())?;
        Ok(Self::with_url(&url, mtm))
    }

    fn with_url(url: &NSURL, mtm: MainThreadMarker) -> Self {
        unsafe {
            let asset = AVURLAsset::URLAssetWithURL_options(url, None);
            let item = AVPlayerItem::playerItemWithAsset(&asset, mtm);
            let output = video_output(None);
            item.addOutput(&output);
            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            player.setActionAtItemEnd(AVPlayerActionAtItemEnd::Pause);
            player.pause();
            Self {
                player,
                item,
                output,
                orientation: None,
                has_video: None,
                serial: 0,
                pending: Some(PendingSeek::new(0, 0.0)),
                seek_generation: 0,
                seek_done: Arc::new(AtomicU64::new(0)),
                waiting_since: None,
            }
        }
    }

    pub fn play(&mut self) {
        if self.state().ended {
            self.seek_exact(0.0);
        }
        unsafe { self.player.play() };
    }

    pub fn pause(&mut self) {
        unsafe { self.player.pause() };
    }

    pub fn seek(&mut self, seconds: f64) {
        let mut target = if seconds.is_finite() {
            seconds.max(0.0)
        } else {
            0.0
        };
        if let Some(duration) = numeric_seconds(unsafe { self.item.duration() }) {
            target = target.min(duration);
        }
        self.seek_exact(target);
    }

    pub fn set_volume(&mut self, volume: f32) {
        let volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        unsafe { self.player.setVolume(volume) };
    }

    pub fn state(&self) -> Playback {
        unsafe {
            let item_status = self.item.status();
            let error = if item_status == AVPlayerItemStatus::Failed {
                Some(error_text(self.item.error(), "media could not be played"))
            } else if self.player.status() == AVPlayerStatus::Failed {
                Some(error_text(self.player.error(), "media player failed"))
            } else {
                None
            };
            let position = numeric_seconds(self.item.currentTime())
                .unwrap_or(0.0)
                .max(0.0);
            let duration = numeric_seconds(self.item.duration()).filter(|d| *d > 0.0);
            let control = self.player.timeControlStatus();
            let playing = error.is_none() && control != AVPlayerTimeControlStatus::Paused;
            let buffering = error.is_none()
                && (item_status == AVPlayerItemStatus::Unknown
                    || playing
                        && control == AVPlayerTimeControlStatus::WaitingToPlayAtSpecifiedRate);
            let has_video = self.has_video.or_else(|| {
                (item_status == AVPlayerItemStatus::ReadyToPlay)
                    .then(|| self.video_track().is_some())
            });
            Playback {
                position: duration.map_or(position, |d| position.min(d)),
                duration,
                playing,
                buffering,
                ended: error.is_none() && playback_ended(position, duration, playing),
                seeking: error.is_none() && self.seeking(has_video),
                error,
            }
        }
    }

    pub fn take_frame(&mut self) -> Option<Frame> {
        self.start_when_buffered();
        let orientation = self.ready_orientation()?;
        // 완료를 프레임보다 먼저 읽는다 — 완료 뒤에 꺼낸 프레임이어야 새 위치의 것이다. 앱에선 새
        // 위치의 프레임이 완료 콜백보다 먼저 오기도 해서, 표시 시각이 목표에 닿은 프레임도 끝으로 친다.
        let settled = self.pending.as_ref().is_none_or(|p| self.settled(p));
        unsafe {
            let item_time = self
                .output
                .itemTimeForMachAbsoluteTime(mach_absolute_time() as i64);
            if !item_time.flags.contains(CMTimeFlags::Valid)
                || !self.output.hasNewPixelBufferForItemTime(item_time)
            {
                return None;
            }
            let mut shown = item_time;
            let buffer = self
                .output
                .copyPixelBufferForItemTime_itemTimeForDisplay(item_time, &mut shown)?;
            let (width, height, rgba) = read_bgra(&buffer, orientation)?;
            let landed = self.pending.as_ref().is_some_and(|p| {
                numeric_seconds(shown).is_some_and(|t| (t - p.target).abs() <= LANDED_SLACK)
            });
            if settled || landed {
                self.pending = None;
            }
            self.serial += 1;
            Some(Frame {
                width,
                height,
                rgba,
                serial: self.serial,
            })
        }
    }

    /// 끊김 최소화 대기는 점진 받기에서 지나치게 오래 간다 — 음성만 있으면 비트레이트의 20배로
    /// 보내도 파일을 끝까지 받은 뒤에야 풀렸고, 그동안 받힌 구간도 0초로만 알렸다(느린 루프백 원본
    /// 실측). 영상은 앞쪽이 충분히 받히면, 음성은 잠깐 기다린 뒤 직접 시작시킨다. 대기 설정은 그대로
    /// 두므로 나중에 받기가 밀리면 AVPlayer 가 다시 기다린다 — 대기를 아예 끄면 한 번 끊긴 뒤 멈춘
    /// 채로 남는다.
    fn start_when_buffered(&mut self) {
        unsafe {
            if self.player.timeControlStatus()
                != AVPlayerTimeControlStatus::WaitingToPlayAtSpecifiedRate
                || self.item.status() != AVPlayerItemStatus::ReadyToPlay
            {
                self.waiting_since = None;
                return;
            }
            let since = *self.waiting_since.get_or_insert_with(Instant::now);
            let Some(now) = numeric_seconds(self.item.currentTime()) else {
                return;
            };
            let end = numeric_seconds(self.item.duration()).unwrap_or(f64::INFINITY);
            let loaded = buffered_until(&self.item.loadedTimeRanges(), now);
            let audio_waited =
                self.has_video == Some(false) && since.elapsed() >= AUDIO_START_WAIT;
            if loaded >= (now + START_AHEAD).min(end - 0.05) || audio_waited {
                self.player.playImmediatelyAtRate(1.0);
                self.waiting_since = None;
            }
        }
    }

    pub fn drop_watch(&self) -> DropWatch {
        DropWatch {
            player: Weak::from_retained(&self.player),
            item: Weak::from_retained(&self.item),
        }
    }

    fn seek_exact(&mut self, seconds: f64) {
        self.seek_generation += 1;
        let generation = self.seek_generation;
        self.pending = Some(PendingSeek::new(generation, seconds));
        let done = Arc::clone(&self.seek_done);
        // 앞선 요청은 `finished = NO` 로 먼저 불린다. 세대가 낮아 지금 기다리는 것에 영향이 없다.
        let completion = block2::RcBlock::new(move |_finished: Bool| {
            done.fetch_max(generation, Ordering::AcqRel);
        });
        unsafe {
            let time = CMTime::with_seconds(seconds, SEEK_TIMESCALE);
            self.player
                .seekToTime_toleranceBefore_toleranceAfter_completionHandler(
                    time,
                    kCMTimeZero,
                    kCMTimeZero,
                    &completion,
                );
        }
    }

    /// 열기는 항목이 준비될 때, 위치 찾기는 완료 콜백이 올 때 끝난다.
    fn settled(&self, pending: &PendingSeek) -> bool {
        let done = if pending.generation == 0 {
            unsafe { self.item.status() == AVPlayerItemStatus::ReadyToPlay }
        } else {
            self.seek_done.load(Ordering::Acquire) >= pending.generation
        };
        if done && pending.settled_at.get().is_none() {
            pending.settled_at.set(Some(Instant::now()));
        }
        done
    }

    /// 비디오면 완료 뒤 첫 프레임을 꺼낼 때(`take_frame`)까지, 소리만이면 완료 콜백까지.
    fn seeking(&self, has_video: Option<bool>) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        if !self.settled(pending) {
            return true;
        }
        has_video != Some(false)
            && pending
                .settled_at
                .get()
                .is_none_or(|at| at.elapsed() <= SEEK_GIVE_UP)
    }

    unsafe fn video_track(&self) -> Option<Retained<AVAssetTrack>> {
        let video = AVMediaTypeVideo?;
        self.item
            .tracks()
            .iter()
            .filter_map(|track| track.assetTrack())
            .find(|track| *track.mediaType() == *video)
    }

    /// 항목이 준비되면 비디오 트랙의 표시 회전을 읽고, 원본이 크면 줄여 받는 출력으로 바꾼다.
    /// 준비 전 트랙 속성을 읽으면 메인 스레드가 파일 읽기를 기다리게 되므로 상태부터 본다.
    fn ready_orientation(&mut self) -> Option<Orientation> {
        if let Some(o) = self.orientation {
            return Some(o);
        }
        unsafe {
            if self.item.status() != AVPlayerItemStatus::ReadyToPlay {
                return None;
            }
            let found = self.video_track();
            self.has_video = Some(found.is_some());
            let orientation = match found {
                Some(track) => {
                    let o = track_orientation(&track);
                    let natural = track.naturalSize();
                    if let Some(size) = scaled_size(natural.width, natural.height) {
                        let smaller = video_output(Some(size));
                        self.item.removeOutput(&self.output);
                        self.item.addOutput(&smaller);
                        self.output = smaller;
                    }
                    o
                }
                None => Orientation::IDENTITY,
            };
            self.orientation = Some(orientation);
            Some(orientation)
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe {
            self.player.pause();
            self.item.cancelPendingSeeks();
            self.item.removeOutput(&self.output);
            self.player.replaceCurrentItemWithPlayerItem(None);
        }
    }
}

/// `Player` 를 떨어뜨린 뒤 AVFoundation 이 객체를 실제로 놓았는지 본다.
pub struct DropWatch {
    player: Weak<AVPlayer>,
    item: Weak<AVPlayerItem>,
}

impl DropWatch {
    pub fn released(&self) -> bool {
        self.player.load().is_none() && self.item.load().is_none()
    }
}

pub fn probe(path: &Path) -> Result<Probe, String> {
    autoreleasepool(|_| probe_file(path))
}

fn probe_file(path: &Path) -> Result<Probe, String> {
    let path = local_file(path)?;
    let url = file_url(&path)?;
    unsafe {
        let precise = NSNumber::new_bool(true);
        let options = NSDictionary::<NSString, AnyObject>::from_slices(
            &[AVURLAssetPreferPreciseDurationAndTimingKey],
            &[&*precise as &AnyObject],
        );
        let asset = AVURLAsset::URLAssetWithURL_options(&url, Some(&options));
        load_keys(&asset, &["playable", "tracks", "duration"])?;
        if !asset.isPlayable() {
            return Err("file is not playable audio or video".into());
        }
        let (video_type, audio_type) = match (AVMediaTypeVideo, AVMediaTypeAudio) {
            (Some(v), Some(a)) => (v, a),
            _ => return Err("AVFoundation media types are unavailable".into()),
        };
        let tracks = asset.tracks();
        let video = tracks.iter().find(|t| *t.mediaType() == *video_type);
        let has_audio = tracks.iter().any(|t| *t.mediaType() == *audio_type);
        if video.is_none() && !has_audio {
            return Err("file has no audio or video track".into());
        }
        let duration_s = numeric_seconds(asset.duration()).filter(|d| *d > 0.0);
        let mut probe = Probe {
            duration_ms: duration_s.map(|d| (d * 1000.0).round() as u64),
            has_audio,
            has_video: video.is_some(),
            ..Probe::default()
        };
        if let Some(track) = video {
            let natural = track.naturalSize();
            let (w, h) = track_orientation(&track).output_size(
                natural.width.round().max(0.0) as u32,
                natural.height.round().max(0.0) as u32,
            );
            if w > 0 && h > 0 {
                probe.width = Some(w);
                probe.height = Some(h);
            }
            probe.poster = poster_png(&asset, duration_s.unwrap_or(0.0));
        }
        Ok(probe)
    }
}

unsafe fn load_keys(asset: &AVAsset, keys: &[&str]) -> Result<(), String> {
    let names: Vec<Retained<NSString>> = keys.iter().map(|k| NSString::from_str(k)).collect();
    let refs: Vec<&NSString> = names.iter().map(|n| &**n).collect();
    let array = NSArray::from_slice(&refs);
    let (tx, rx) = mpsc::channel::<()>();
    let done = block2::RcBlock::new(move || {
        let _ = tx.send(());
    });
    asset.loadValuesAsynchronouslyForKeys_completionHandler(&array, Some(&done));
    if rx.recv_timeout(PROBE_TIMEOUT).is_err() {
        asset.cancelLoading();
        return Err("timed out reading media file".into());
    }
    for key in &names {
        let mut error: Option<Retained<NSError>> = None;
        let status: AVKeyValueStatus =
            msg_send![asset, statusOfValueForKey: &**key, error: &mut error];
        if status != AVKeyValueStatus::Loaded {
            return Err(error_text(error, "could not read media file"));
        }
    }
    Ok(())
}

unsafe fn poster_png(asset: &AVAsset, duration: f64) -> Option<Vec<u8>> {
    let generator = AVAssetImageGenerator::assetImageGeneratorWithAsset(asset);
    generator.setAppliesPreferredTrackTransform(true);
    let edge = POSTER_MAX_EDGE as f64;
    generator.setMaximumSize(CGSize::new(edge, edge));
    // 첫 프레임은 검은 화면인 영상이 많아 조금 들어간 곳을 고른다.
    let at = CMTime::with_seconds((duration * 0.1).min(1.0), SEEK_TIMESCALE);
    #[allow(deprecated)]
    let image = generator
        .copyCGImageAtTime_actualTime_error(at, std::ptr::null_mut())
        .ok()?;
    let png = encode_png(&image)?;
    (png.len() <= POSTER_MAX_BYTES).then_some(png)
}

fn encode_png(image: &CGImage) -> Option<Vec<u8>> {
    let (w, h) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    if w == 0 || h == 0 || w > POSTER_MAX_EDGE as usize || h > POSTER_MAX_EDGE as usize {
        return None;
    }
    let mut rgba = vec![0u8; w * h * 4];
    let space = CGColorSpace::with_name(Some(srgb_name()))?;
    let info = CGImageAlphaInfo::PremultipliedLast.0 | CGImageByteOrderInfo::Order32Big.0;
    let context = unsafe {
        CGBitmapContextCreate(rgba.as_mut_ptr().cast(), w, h, 8, w * 4, Some(&space), info)
    }?;
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64));
    CGContext::draw_image(Some(&context), rect, Some(image));
    drop(context);
    let img = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

fn srgb_name() -> &'static CFString {
    unsafe { objc2_core_graphics::kCGColorSpaceSRGB }
}

unsafe fn track_orientation(track: &AVAssetTrack) -> Orientation {
    let t = track.preferredTransform();
    Orientation::from_affine(t.a, t.b, t.c, t.d)
}

/// 긴 변이 `FRAME_MAX_EDGE` 를 넘을 때만 줄인 크기. 4:2:0 디코더 출력에 맞춰 짝수로 맞춘다.
fn scaled_size(width: f64, height: f64) -> Option<(usize, usize)> {
    let edge = width.max(height);
    if edge.is_nan() || edge <= FRAME_MAX_EDGE || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let scale = FRAME_MAX_EDGE / edge;
    let even = |v: f64| (((v * scale) / 2.0).round() as usize * 2).max(2);
    Some((even(width), even(height)))
}

fn video_output(size: Option<(usize, usize)>) -> Retained<AVPlayerItemVideoOutput> {
    unsafe {
        let mut keys: Vec<&NSString> = vec![cf_key(kCVPixelBufferPixelFormatTypeKey)];
        let mut values: Vec<Retained<NSNumber>> =
            vec![NSNumber::new_u32(kCVPixelFormatType_32BGRA)];
        if let Some((w, h)) = size {
            keys.push(cf_key(kCVPixelBufferWidthKey));
            values.push(NSNumber::new_usize(w));
            keys.push(cf_key(kCVPixelBufferHeightKey));
            values.push(NSNumber::new_usize(h));
        }
        let objects: Vec<&AnyObject> = values.iter().map(|v| &**v as &AnyObject).collect();
        let attributes = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &objects);
        AVPlayerItemVideoOutput::initWithPixelBufferAttributes(
            AVPlayerItemVideoOutput::alloc(),
            Some(&attributes),
        )
    }
}

/// CFString 과 NSString 은 같은 객체(toll-free bridged)다.
fn cf_key(key: &'static CFString) -> &'static NSString {
    unsafe { &*(key as *const CFString).cast::<NSString>() }
}

unsafe fn read_bgra(
    buffer: &CVPixelBuffer,
    orientation: Orientation,
) -> Option<(u32, u32, Vec<u8>)> {
    if CVPixelBufferGetPixelFormatType(buffer) != kCVPixelFormatType_32BGRA {
        return None;
    }
    if CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) != kCVReturnSuccess {
        return None;
    }
    let width = CVPixelBufferGetWidth(buffer);
    let height = CVPixelBufferGetHeight(buffer);
    let stride = CVPixelBufferGetBytesPerRow(buffer);
    let base = CVPixelBufferGetBaseAddress(buffer).cast::<u8>();
    let converted = if base.is_null() || width == 0 || height == 0 {
        None
    } else {
        let src = std::slice::from_raw_parts(base, stride * height);
        orientation.bgra_to_rgba(src, stride, width as u32, height as u32)
    };
    CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly);
    let rgba = converted?;
    let (w, h) = orientation.output_size(width as u32, height as u32);
    Some((w, h, rgba))
}

/// `CMTimeRange`. objc2-core-media 의 같은 타입은 Cargo 기능을 따로 켜야 해서 배치만 맞춰 둔다.
#[repr(C)]
struct TimeRange {
    start: CMTime,
    duration: CMTime,
}

unsafe impl Encode for TimeRange {
    const ENCODING: Encoding = Encoding::Struct("?", &[CMTime::ENCODING, CMTime::ENCODING]);
}

/// `at` 을 품은 받힌 구간이 어디까지 이어지는지. 없으면 `at` 그대로.
fn buffered_until(ranges: &NSArray<NSValue>, at: f64) -> f64 {
    ranges
        .iter()
        .filter_map(|value| {
            let range: TimeRange = unsafe { msg_send![&*value, CMTimeRangeValue] };
            let start = numeric_seconds(range.start)?;
            let end = start + numeric_seconds(range.duration)?;
            (start <= at + 0.05 && end > at).then_some(end)
        })
        .fold(at, f64::max)
}

fn numeric_seconds(time: CMTime) -> Option<f64> {
    let numeric = time.flags.contains(CMTimeFlags::Valid)
        && !time.flags.intersects(
            CMTimeFlags::Indefinite | CMTimeFlags::PositiveInfinity | CMTimeFlags::NegativeInfinity,
        );
    if !numeric {
        return None;
    }
    let s = unsafe { time.seconds() };
    s.is_finite().then_some(s)
}

fn error_text(error: Option<Retained<NSError>>, fallback: &str) -> String {
    error
        .map(|e| e.localizedDescription().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn main_thread() -> Result<MainThreadMarker, String> {
    MainThreadMarker::new().ok_or_else(|| "media player must be opened on the main thread".to_string())
}

fn local_file(path: &Path) -> Result<PathBuf, String> {
    let resolved =
        std::fs::canonicalize(path).map_err(|e| format!("cannot open media file: {e}"))?;
    let meta = std::fs::metadata(&resolved).map_err(|e| format!("cannot open media file: {e}"))?;
    if !meta.is_file() {
        return Err("media path is not a regular file".into());
    }
    Ok(resolved)
}

/// UTF-8 이 아닌 경로도 그대로 넘기려고 파일 시스템 표현으로 URL 을 만든다.
fn file_url(path: &Path) -> Result<Retained<NSURL>, String> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "media path contains a NUL byte".to_string())?;
    let ptr = NonNull::new(c_path.as_ptr().cast_mut()).ok_or("invalid media path")?;
    Ok(unsafe {
        NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(ptr, false, None)
    })
}
