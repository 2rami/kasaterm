//! VideoToolbox 하드웨어 압축·풀기. 설정값의 근거는 `docs/screen-stream.md` 「S0 실측」.

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
use objc2_video_toolbox::{
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxFrameDelayCount,
    kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTDecompressionPropertyKey_RealTime, kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_High_AutoLevel,
    kVTProfileLevel_HEVC_Main_AutoLevel, kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder, VTCompressionSession, VTDecodeFrameFlags,
    VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord, VTDecompressionSession, VTEncodeInfoFlags, VTSessionSetProperty,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Hevc,
    H264,
}

impl Codec {
    pub fn named(s: &str) -> Self {
        if s == "h264" { Self::H264 } else { Self::Hevc }
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
}

pub struct Encoded {
    pub seq: u32,
    pub cap_id: u64,
    pub key: bool,
    pub t_cap: f64,
    pub t_enc: f64,
    pub param_sets: Vec<Vec<u8>>,
    pub data: Vec<u8>,
}

pub struct FrameMeta {
    pub seq: u32,
    pub cap_id: u64,
    pub t_cap: f64,
}

struct EncCtx {
    codec: Codec,
    out: Box<dyn Fn(Encoded) + Send + Sync>,
}

pub struct Encoder {
    session: CFRetained<VTCompressionSession>,
    ctx: *mut EncCtx,
    /// 받아들여지지 않은 설정 — 보는 쪽 정보에 싣는다. 저지연 모드에서는 `MaxFrameDelayCount` 가 늘 여기 든다(S0).
    pub rejected: Vec<String>,
}

unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new(cfg: &EncoderConfig, out: impl Fn(Encoded) + Send + Sync + 'static) -> Result<Self> {
        let ctx = Box::into_raw(Box::new(EncCtx { codec: cfg.codec, out: Box::new(out) }));
        let mut rejected = Vec::new();
        // 저지연 율 제어가 없는 인코더면 그것만 빼고 다시 — 끄면 p90 이 튀고 장이 커질 뿐 보이기는 한다.
        let session = match create_session(cfg, ctx, true) {
            Ok(s) => s,
            Err(status) => {
                rejected.push(format!("EnableLowLatencyRateControl({status})"));
                match create_session(cfg, ctx, false) {
                    Ok(s) => s,
                    Err(status) => {
                        drop(unsafe { Box::from_raw(ctx) });
                        bail!("인코더 세션 생성 실패 {status}");
                    }
                }
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
            set("RealTime", kVTCompressionPropertyKey_RealTime, t);
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
        }
        unsafe { session.prepare_to_encode_frames() };
        Ok(Self { session, ctx, rejected })
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
    let t_enc = super::host_now();
    let sample = unsafe { &*sample };
    let key = unsafe { is_sync(sample) };
    let param_sets = if key { unsafe { param_sets(sample, ctx.codec) } } else { Vec::new() };
    let Some(data) = (unsafe { sample_bytes(sample) }) else { return };
    (ctx.out)(Encoded {
        seq: meta.seq,
        cap_id: meta.cap_id,
        key,
        t_cap: meta.t_cap,
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

/// 풀린 BGRA 장을 잠근 채 바이트로 넘긴다(가로·세로·줄 간격). 줄 간격을 그대로 넘겨 바꿔 담지 않는다.
pub fn with_bgra<R>(px: &CVPixelBuffer, f: impl FnOnce(&[u8], u32, u32, u32) -> R) -> Option<R> {
    use objc2_core_video::{
        CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
        CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };
    if CVPixelBufferGetPixelFormatType(px) != kCVPixelFormatType_32BGRA {
        return None;
    }
    unsafe { CVPixelBufferLockBaseAddress(px, CVPixelBufferLockFlags::ReadOnly) };
    let (w, h, stride) = (CVPixelBufferGetWidth(px), CVPixelBufferGetHeight(px), CVPixelBufferGetBytesPerRow(px));
    let base = CVPixelBufferGetBaseAddress(px).cast::<u8>();
    let out = (!base.is_null() && w > 0 && h > 0).then(|| {
        let bytes = unsafe { std::slice::from_raw_parts(base, stride * h) };
        f(bytes, w as u32, h as u32, stride as u32)
    });
    unsafe { CVPixelBufferUnlockBaseAddress(px, CVPixelBufferLockFlags::ReadOnly) };
    out
}
