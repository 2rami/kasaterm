use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Duration;

use objc2::rc::{autoreleasepool, Retained, Weak};
use objc2::runtime::AnyObject;
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
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString, NSURL};

use super::{
    playback_ended, Frame, Orientation, Playback, Probe, POSTER_MAX_BYTES, POSTER_MAX_EDGE,
};

/// 프레임을 매번 CPU 로 RGBA 변환하므로 4K 원본은 이 긴 변으로 줄여 받는다(VideoToolbox 가 줄인다).
const FRAME_MAX_EDGE: f64 = 1920.0;
/// 정확한 위치 찾기에 쓰는 시간 단위. 600 은 24·25·30·60fps 를 모두 나눠 떨어뜨린다.
const SEEK_TIMESCALE: i32 = 600;
/// 깨진 파일·느린 볼륨에서 검사 스레드가 영영 붙잡히지 않게.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

extern "C" {
    fn mach_absolute_time() -> u64;
}

pub struct Player {
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    output: Retained<AVPlayerItemVideoOutput>,
    /// 항목이 준비된 뒤 한 번 정한다. 그 전엔 프레임을 내지 않는다.
    orientation: Option<Orientation>,
    serial: u64,
}

impl Player {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| "media player must be opened on the main thread".to_string())?;
        let path = local_file(path)?;
        let url = file_url(&path)?;
        unsafe {
            let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
            let item = AVPlayerItem::playerItemWithAsset(&asset, mtm);
            let output = video_output(None);
            item.addOutput(&output);
            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            player.setActionAtItemEnd(AVPlayerActionAtItemEnd::Pause);
            player.pause();
            Ok(Self {
                player,
                item,
                output,
                orientation: None,
                serial: 0,
            })
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
            let buffering =
                playing && control == AVPlayerTimeControlStatus::WaitingToPlayAtSpecifiedRate;
            Playback {
                position: duration.map_or(position, |d| position.min(d)),
                duration,
                playing,
                buffering,
                ended: error.is_none() && playback_ended(position, duration, playing),
                error,
            }
        }
    }

    pub fn take_frame(&mut self) -> Option<Frame> {
        let orientation = self.ready_orientation()?;
        unsafe {
            let item_time = self
                .output
                .itemTimeForMachAbsoluteTime(mach_absolute_time() as i64);
            if !item_time.flags.contains(CMTimeFlags::Valid)
                || !self.output.hasNewPixelBufferForItemTime(item_time)
            {
                return None;
            }
            let buffer = self
                .output
                .copyPixelBufferForItemTime_itemTimeForDisplay(item_time, std::ptr::null_mut())?;
            let (width, height, rgba) = read_bgra(&buffer, orientation)?;
            self.serial += 1;
            Some(Frame {
                width,
                height,
                rgba,
                serial: self.serial,
            })
        }
    }

    pub fn drop_watch(&self) -> DropWatch {
        DropWatch {
            player: Weak::from_retained(&self.player),
            item: Weak::from_retained(&self.item),
        }
    }

    fn seek_exact(&mut self, seconds: f64) {
        unsafe {
            let time = CMTime::with_seconds(seconds, SEEK_TIMESCALE);
            self.player
                .seekToTime_toleranceBefore_toleranceAfter(time, kCMTimeZero, kCMTimeZero);
        }
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
            let video = AVMediaTypeVideo?;
            let mut found = None;
            for track in self.item.tracks().iter() {
                let Some(asset_track) = track.assetTrack() else {
                    continue;
                };
                if *asset_track.mediaType() == *video {
                    found = Some(asset_track);
                    break;
                }
            }
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
