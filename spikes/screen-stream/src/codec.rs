//! VideoToolbox 하드웨어 압축·풀기.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Mutex;

use anyhow::{bail, Result};
use objc2_core_foundation::{
    kCFAllocatorNull, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    kCMSampleAttachmentKey_NotSync, kCMTimeInvalid, kCMVideoCodecType_H264, kCMVideoCodecType_HEVC,
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime,
};
use objc2_core_video::{
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA,
    CVImageBuffer, CVPixelBuffer,
};
#[allow(deprecated)]
use objc2_core_media::{
    CMVideoFormatDescriptionCreateFromH264ParameterSets, CMVideoFormatDescriptionCreateFromHEVCParameterSets,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex, CMVideoFormatDescriptionGetHEVCParameterSetAtIndex,
};
use objc2_video_toolbox::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Hevc,
    H264,
}

impl Codec {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "hevc" | "h265" => Self::Hevc,
            "h264" | "avc" => Self::H264,
            other => bail!("모르는 코덱 {other}"),
        })
    }

    fn cm(self) -> u32 {
        match self {
            Self::Hevc => kCMVideoCodecType_HEVC,
            Self::H264 => kCMVideoCodecType_H264,
        }
    }
}

pub struct EncoderConfig {
    pub codec: Codec,
    pub width: usize,
    pub height: usize,
    pub bitrate: i64,
    pub fps: u32,
    pub low_latency: bool,
    pub speed_over_quality: bool,
    pub realtime: bool,
    /// 장마다 `complete_frames` 로 바로 내보내게 한다.
    pub flush: bool,
}

pub struct Encoded {
    pub seq: u32,
    pub cap_id: u64,
    pub key: bool,
    pub t_cap: f64,
    pub t_cb: f64,
    pub t_enc: f64,
    pub param_sets: Vec<Vec<u8>>,
    pub data: Vec<u8>,
}

pub struct FrameMeta {
    pub seq: u32,
    pub cap_id: u64,
    pub t_cap: f64,
    pub t_cb: f64,
}

struct EncCtx {
    codec: Codec,
    out: Box<dyn Fn(Encoded) + Send + Sync>,
}

pub struct Encoder {
    session: CFRetained<VTCompressionSession>,
    ctx: *mut EncCtx,
    /// 받아들여지지 않은 설정 — 측정 보고에 싣는다.
    pub rejected: Vec<String>,
    pub hardware: Option<bool>,
    flush: bool,
}

unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new(cfg: &EncoderConfig, out: impl Fn(Encoded) + Send + Sync + 'static) -> Result<Self> {
        let ctx = Box::into_raw(Box::new(EncCtx { codec: cfg.codec, out: Box::new(out) }));
        let mut rejected = Vec::new();
        let session = match create_session(cfg, ctx, cfg.low_latency) {
            Ok(s) => s,
            Err(status) if cfg.low_latency => {
                rejected.push(format!("EnableLowLatencyRateControl(세션 생성 {status})"));
                create_session(cfg, ctx, false).map_err(|s| anyhow::anyhow!("인코더 세션 생성 실패 {s}"))?
            }
            Err(status) => {
                drop(unsafe { Box::from_raw(ctx) });
                bail!("인코더 세션 생성 실패 {status}");
            }
        };
        let t = CFBoolean::new(true);
        let f = CFBoolean::new(false);
        let mut set = |name: &str, key: &CFString, value: &CFType| {
            let status = unsafe { VTSessionSetProperty(&session, key, Some(value)) };
            if status != 0 {
                rejected.push(format!("{name}({status})"));
            }
        };
        unsafe {
            set("RealTime", kVTCompressionPropertyKey_RealTime, if cfg.realtime { t } else { f });
            set("AllowFrameReordering", kVTCompressionPropertyKey_AllowFrameReordering, f);
            let profile = match cfg.codec {
                Codec::Hevc => kVTProfileLevel_HEVC_Main_AutoLevel,
                Codec::H264 => kVTProfileLevel_H264_High_AutoLevel,
            };
            set("ProfileLevel", kVTCompressionPropertyKey_ProfileLevel, profile);
            set("AverageBitRate", kVTCompressionPropertyKey_AverageBitRate, &CFNumber::new_i64(cfg.bitrate));
            set("ExpectedFrameRate", kVTCompressionPropertyKey_ExpectedFrameRate, &CFNumber::new_i32(cfg.fps as i32));
            // 키프레임은 받는 쪽이 청할 때만 — 주기 키프레임은 큰 장이 주기적으로 지연을 튀게 한다.
            set("MaxKeyFrameInterval", kVTCompressionPropertyKey_MaxKeyFrameInterval, &CFNumber::new_i32(i32::MAX));
            set("MaxFrameDelayCount", kVTCompressionPropertyKey_MaxFrameDelayCount, &CFNumber::new_i32(0));
            if cfg.speed_over_quality {
                set(
                    "PrioritizeEncodingSpeedOverQuality",
                    kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
                    t,
                );
            }
        }
        unsafe { session.prepare_to_encode_frames() };
        let hardware = unsafe { copy_bool(&session, kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder) };
        Ok(Self { session, ctx, rejected, hardware, flush: cfg.flush })
    }

    /// `pts` 는 보낼 때마다 늘어야 한다 — 같은 그림을 다시 보낼 때도 새 시각을 준다.
    pub fn encode(&self, pixels: &CVPixelBuffer, meta: FrameMeta, force_key: bool, pts: f64) {
        let pts = unsafe { CMTime::with_seconds(pts, 1_000_000) };
        let props = force_key.then(|| {
            CFDictionary::<CFString, CFBoolean>::from_slices(
                &[unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame }],
                &[CFBoolean::new(true)],
            )
        });
        let meta = Box::into_raw(Box::new(meta));
        let status = unsafe {
            self.session.encode_frame(
                pixels,
                pts,
                kCMTimeInvalid,
                props.as_deref().map(|d| d.as_opaque()),
                meta.cast(),
                ptr::null_mut(),
            )
        };
        if status == 0 && self.flush {
            unsafe { self.session.complete_frames(pts) };
        }
        if status != 0 {
            // 실패한 장은 콜백이 오지 않는다.
            drop(unsafe { Box::from_raw(meta) });
            eprintln!("encode_frame 실패 {status}");
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            self.session.complete_frames(kCMTimeInvalid);
            self.session.invalidate();
            drop(Box::from_raw(self.ctx));
        }
    }
}

fn create_session(cfg: &EncoderConfig, ctx: *mut EncCtx, low_latency: bool) -> Result<CFRetained<VTCompressionSession>, i32> {
    let t = CFBoolean::new(true);
    let mut keys: Vec<&CFString> = vec![unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder }];
    let mut vals: Vec<&CFBoolean> = vec![t];
    if low_latency {
        keys.push(unsafe { kVTVideoEncoderSpecification_EnableLowLatencyRateControl });
        vals.push(t);
    }
    let spec = CFDictionary::<CFString, CFBoolean>::from_slices(&keys, &vals);
    let mut out: *mut VTCompressionSession = ptr::null_mut();
    let status = unsafe {
        VTCompressionSession::create(
            None,
            cfg.width as i32,
            cfg.height as i32,
            cfg.codec.cm(),
            Some(spec.as_opaque()),
            None,
            None,
            Some(on_encoded),
            ctx.cast(),
            NonNull::from(&mut out),
        )
    };
    match NonNull::new(out) {
        Some(p) if status == 0 => Ok(unsafe { CFRetained::from_raw(p) }),
        _ => Err(status),
    }
}

unsafe fn copy_bool(session: &VTCompressionSession, key: &CFString) -> Option<bool> {
    let mut out: *const c_void = ptr::null();
    let status = unsafe { VTSessionCopyProperty(session, key, None, (&mut out as *mut *const c_void).cast()) };
    if status != 0 || out.is_null() {
        return None;
    }
    let b: CFRetained<CFBoolean> = unsafe { CFRetained::from_raw(NonNull::new_unchecked(out as *mut CFBoolean)) };
    Some(b.as_bool())
}

unsafe extern "C-unwind" fn on_encoded(
    refcon: *mut c_void,
    frame_ref: *mut c_void,
    status: i32,
    _flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    let meta = unsafe { Box::from_raw(frame_ref.cast::<FrameMeta>()) };
    let ctx = unsafe { &*refcon.cast::<EncCtx>() };
    if status != 0 || sample.is_null() {
        eprintln!("인코더가 장을 버렸다 seq={} status={status}", meta.seq);
        return;
    }
    let t_enc = crate::host_now();
    let sample = unsafe { &*sample };
    let key = unsafe { is_sync(sample) };
    let param_sets = if key { unsafe { param_sets(sample, ctx.codec) } } else { Vec::new() };
    let Some(data) = (unsafe { sample_bytes(sample) }) else { return };
    (ctx.out)(Encoded {
        seq: meta.seq,
        cap_id: meta.cap_id,
        key,
        t_cap: meta.t_cap,
        t_cb: meta.t_cb,
        t_enc,
        param_sets,
        data,
    });
}

unsafe fn is_sync(sample: &CMSampleBuffer) -> bool {
    let Some(arr) = (unsafe { sample.sample_attachments_array(false) }) else { return true };
    if arr.count() < 1 {
        return true;
    }
    let dict = unsafe { arr.value_at_index(0) }.cast::<CFDictionary>();
    if dict.is_null() {
        return true;
    }
    let key: &CFString = unsafe { kCMSampleAttachmentKey_NotSync };
    let v = unsafe { (*dict).value((key as *const CFString).cast()) };
    if v.is_null() {
        return true;
    }
    !unsafe { &*v.cast::<CFBoolean>() }.as_bool()
}

#[allow(deprecated)]
unsafe fn param_sets(sample: &CMSampleBuffer, codec: Codec) -> Vec<Vec<u8>> {
    let Some(desc) = (unsafe { sample.format_description() }) else { return Vec::new() };
    let get = |i: usize, p: *mut *const u8, size: *mut usize, count: *mut usize| unsafe {
        let mut nal = 0;
        match codec {
            Codec::Hevc => CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(&desc, i, p, size, count, &mut nal),
            Codec::H264 => CMVideoFormatDescriptionGetH264ParameterSetAtIndex(&desc, i, p, size, count, &mut nal),
        }
    };
    let mut count = 0usize;
    if get(0, ptr::null_mut(), ptr::null_mut(), &mut count) != 0 {
        return Vec::new();
    }
    (0..count)
        .filter_map(|i| {
            let mut p: *const u8 = ptr::null();
            let mut size = 0usize;
            (get(i, &mut p, &mut size, ptr::null_mut()) == 0 && !p.is_null())
                .then(|| unsafe { std::slice::from_raw_parts(p, size) }.to_vec())
        })
        .collect()
}

unsafe fn sample_bytes(sample: &CMSampleBuffer) -> Option<Vec<u8>> {
    let block = unsafe { sample.data_buffer() }?;
    let len = unsafe { block.data_length() };
    let mut data = vec![0u8; len];
    let status = unsafe { block.copy_data_bytes(0, len, NonNull::new(data.as_mut_ptr().cast())?) };
    (status == 0).then_some(data)
}

/// 받는 쪽. 키프레임의 파라미터 셋이 바뀌면 세션을 새로 만든다.
pub struct Decoder {
    codec: Codec,
    param_sets: Vec<Vec<u8>>,
    format: Option<CFRetained<CMFormatDescription>>,
    session: Option<CFRetained<VTDecompressionSession>>,
    ctx: Box<DecCtx>,
}

#[derive(Default)]
struct DecCtx {
    last: Mutex<Option<CFRetained<CVPixelBuffer>>>,
    failed: Mutex<Option<i32>>,
}

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(codec: Codec) -> Self {
        Self { codec, param_sets: Vec::new(), format: None, session: None, ctx: Box::default() }
    }

    /// 한 장을 끝까지 풀고 돌아온다(동기). 풀린 그림은 `last` 로 꺼낸다.
    pub fn decode(&mut self, param_sets: &[Vec<u8>], data: &mut [u8], pts: f64) -> Result<()> {
        if !param_sets.is_empty() && param_sets != self.param_sets.as_slice() {
            self.reset(param_sets)?;
        }
        let (Some(format), Some(session)) = (&self.format, &self.session) else {
            bail!("키프레임 전의 장");
        };
        let mut block: *mut CMBlockBuffer = ptr::null_mut();
        let status = unsafe {
            CMBlockBuffer::create_with_memory_block(
                None,
                data.as_mut_ptr().cast(),
                data.len(),
                kCFAllocatorNull,
                ptr::null(),
                0,
                data.len(),
                0,
                NonNull::from(&mut block),
            )
        };
        let Some(block) = NonNull::new(block).filter(|_| status == 0) else { bail!("CMBlockBuffer {status}") };
        let block: CFRetained<CMBlockBuffer> = unsafe { CFRetained::from_raw(block) };
        let timing = CMSampleTimingInfo {
            duration: unsafe { kCMTimeInvalid },
            presentationTimeStamp: unsafe { CMTime::with_seconds(pts, 1_000_000) },
            decodeTimeStamp: unsafe { kCMTimeInvalid },
        };
        let size = data.len();
        let mut sample: *mut CMSampleBuffer = ptr::null_mut();
        let status = unsafe {
            CMSampleBuffer::create_ready(None, Some(&block), Some(format), 1, 1, &timing, 1, &size, NonNull::from(&mut sample))
        };
        let Some(sample) = NonNull::new(sample).filter(|_| status == 0) else { bail!("CMSampleBuffer {status}") };
        let sample: CFRetained<CMSampleBuffer> = unsafe { CFRetained::from_raw(sample) };
        *self.ctx.failed.lock().unwrap() = None;
        let status = unsafe {
            session.decode_frame(&sample, VTDecodeFrameFlags(0), ptr::null_mut(), ptr::null_mut())
        };
        if status != 0 {
            bail!("decode_frame {status}");
        }
        unsafe { session.wait_for_asynchronous_frames() };
        if let Some(s) = *self.ctx.failed.lock().unwrap() {
            bail!("디코드 콜백 {s}");
        }
        Ok(())
    }

    pub fn last(&self) -> Option<CFRetained<CVPixelBuffer>> {
        self.ctx.last.lock().unwrap().clone()
    }

    #[allow(deprecated)]
    fn reset(&mut self, param_sets: &[Vec<u8>]) -> Result<()> {
        if let Some(s) = self.session.take() {
            unsafe { s.invalidate() };
        }
        let mut ptrs: Vec<NonNull<u8>> = param_sets.iter().map(|p| NonNull::new(p.as_ptr() as *mut u8).unwrap()).collect();
        let mut sizes: Vec<usize> = param_sets.iter().map(Vec::len).collect();
        let mut fmt: *const CMFormatDescription = ptr::null();
        let status = unsafe {
            match self.codec {
                Codec::Hevc => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    None,
                    ptrs.len(),
                    NonNull::new(ptrs.as_mut_ptr()).unwrap(),
                    NonNull::new(sizes.as_mut_ptr()).unwrap(),
                    4,
                    None,
                    NonNull::from(&mut fmt),
                ),
                Codec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    None,
                    ptrs.len(),
                    NonNull::new(ptrs.as_mut_ptr()).unwrap(),
                    NonNull::new(sizes.as_mut_ptr()).unwrap(),
                    4,
                    NonNull::from(&mut fmt),
                ),
            }
        };
        let Some(fmt) = NonNull::new(fmt as *mut CMFormatDescription).filter(|_| status == 0) else {
            bail!("포맷 설명 {status}");
        };
        let format: CFRetained<CMFormatDescription> = unsafe { CFRetained::from_raw(fmt) };

        let surface = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let bgra = CFNumber::new_i32(kCVPixelFormatType_32BGRA as i32);
        let attrs = CFDictionary::<CFString, CFType>::from_slices(
            &[unsafe { kCVPixelBufferPixelFormatTypeKey }, unsafe { kCVPixelBufferIOSurfacePropertiesKey }],
            &[&bgra, &surface],
        );
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(on_decoded),
            decompressionOutputRefCon: (&*self.ctx as *const DecCtx as *mut DecCtx).cast(),
        };
        let mut out: *mut VTDecompressionSession = ptr::null_mut();
        let status = unsafe {
            VTDecompressionSession::create(None, &format, None, Some(attrs.as_opaque()), &record, NonNull::from(&mut out))
        };
        let Some(out) = NonNull::new(out).filter(|_| status == 0) else { bail!("디코더 세션 {status}") };
        let session: CFRetained<VTDecompressionSession> = unsafe { CFRetained::from_raw(out) };
        unsafe { VTSessionSetProperty(&session, kVTDecompressionPropertyKey_RealTime, Some(CFBoolean::new(true))) };
        self.format = Some(format);
        self.session = Some(session);
        self.param_sets = param_sets.to_vec();
        Ok(())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if let Some(s) = self.session.take() {
            unsafe { s.invalidate() };
        }
    }
}

unsafe extern "C-unwind" fn on_decoded(
    refcon: *mut c_void,
    _frame_ref: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _pts: CMTime,
    _dur: CMTime,
) {
    let ctx = unsafe { &*refcon.cast::<DecCtx>() };
    if status != 0 || image.is_null() {
        *ctx.failed.lock().unwrap() = Some(status);
        return;
    }
    let image = unsafe { CFRetained::retain(NonNull::new_unchecked(image)) };
    *ctx.last.lock().unwrap() = Some(image);
}

/// BGRA 픽셀 버퍼를 RGB 바이트로(PNG·화질 비교용).
pub fn bgra_to_rgb(pixels: &CVPixelBuffer) -> Option<(Vec<u8>, u32, u32)> {
    use objc2_core_video::{
        CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
        CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };
    if CVPixelBufferGetPixelFormatType(pixels) != kCVPixelFormatType_32BGRA {
        return None;
    }
    unsafe { CVPixelBufferLockBaseAddress(pixels, CVPixelBufferLockFlags::ReadOnly) };
    let w = CVPixelBufferGetWidth(pixels);
    let h = CVPixelBufferGetHeight(pixels);
    let stride = CVPixelBufferGetBytesPerRow(pixels);
    let base = CVPixelBufferGetBaseAddress(pixels).cast::<u8>();
    let mut rgb = Vec::with_capacity(w * h * 3);
    if !base.is_null() {
        for y in 0..h {
            let row = unsafe { std::slice::from_raw_parts(base.add(y * stride), w * 4) };
            for p in row.chunks_exact(4) {
                rgb.extend_from_slice(&[p[2], p[1], p[0]]);
            }
        }
    }
    unsafe { CVPixelBufferUnlockBaseAddress(pixels, CVPixelBufferLockFlags::ReadOnly) };
    (!base.is_null()).then_some((rgb, w as u32, h as u32))
}

/// 움직임 시험용 — 화면 한 장을 장마다 `step` 줄씩 위로 밀어(끝은 처음과 이어 붙여) 문서를 계속 스크롤하는 화면을 만든다.
/// 사람 화면을 건드리지 않고 화면 가득 글자가 움직이는 경우를 잰다.
pub struct Scroller {
    pool: CFRetained<objc2_core_video::CVPixelBufferPool>,
    offset: usize,
}

unsafe impl Send for Scroller {}

impl Scroller {
    pub fn new(width: usize, height: usize) -> Result<Self> {
        use objc2_core_video::{kCVPixelBufferHeightKey, kCVPixelBufferWidthKey, CVPixelBufferPool};
        let surface = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let bgra = CFNumber::new_i32(kCVPixelFormatType_32BGRA as i32);
        let w = CFNumber::new_i64(width as i64);
        let h = CFNumber::new_i64(height as i64);
        let attrs = CFDictionary::<CFString, CFType>::from_slices(
            &[
                unsafe { kCVPixelBufferPixelFormatTypeKey },
                unsafe { kCVPixelBufferIOSurfacePropertiesKey },
                unsafe { kCVPixelBufferWidthKey },
                unsafe { kCVPixelBufferHeightKey },
            ],
            &[&bgra, &surface, &w, &h],
        );
        let mut out: *mut CVPixelBufferPool = ptr::null_mut();
        let status = unsafe { CVPixelBufferPool::create(None, None, Some(attrs.as_opaque()), NonNull::from(&mut out)) };
        let Some(out) = NonNull::new(out).filter(|_| status == 0) else { bail!("픽셀 버퍼 풀 {status}") };
        Ok(Self { pool: unsafe { CFRetained::from_raw(out) }, offset: 0 })
    }

    pub fn next(&mut self, base: &CVPixelBuffer, step: usize) -> Option<CFRetained<CVPixelBuffer>> {
        use objc2_core_video::{
            CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType,
            CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferPool,
            CVPixelBufferUnlockBaseAddress,
        };
        if CVPixelBufferGetPixelFormatType(base) != kCVPixelFormatType_32BGRA {
            return None;
        }
        let mut out: *mut CVPixelBuffer = ptr::null_mut();
        let status = unsafe { CVPixelBufferPool::create_pixel_buffer(None, &self.pool, NonNull::from(&mut out)) };
        let out: CFRetained<CVPixelBuffer> = unsafe { CFRetained::from_raw(NonNull::new(out).filter(|_| status == 0)?) };
        let (w, h) = (CVPixelBufferGetWidth(base), CVPixelBufferGetHeight(base));
        if (CVPixelBufferGetWidth(&out), CVPixelBufferGetHeight(&out)) != (w, h) {
            return None;
        }
        self.offset = (self.offset + step) % h;
        unsafe {
            CVPixelBufferLockBaseAddress(base, CVPixelBufferLockFlags::ReadOnly);
            CVPixelBufferLockBaseAddress(&out, CVPixelBufferLockFlags(0));
        }
        let (src, dst) = (CVPixelBufferGetBaseAddress(base).cast::<u8>(), CVPixelBufferGetBaseAddress(&out).cast::<u8>());
        let (ss, ds) = (CVPixelBufferGetBytesPerRow(base), CVPixelBufferGetBytesPerRow(&out));
        if !src.is_null() && !dst.is_null() {
            for y in 0..h {
                let from = (y + self.offset) % h;
                unsafe { ptr::copy_nonoverlapping(src.add(from * ss), dst.add(y * ds), w * 4) };
            }
        }
        unsafe {
            CVPixelBufferUnlockBaseAddress(&out, CVPixelBufferLockFlags(0));
            CVPixelBufferUnlockBaseAddress(base, CVPixelBufferLockFlags::ReadOnly);
        }
        Some(out)
    }
}
