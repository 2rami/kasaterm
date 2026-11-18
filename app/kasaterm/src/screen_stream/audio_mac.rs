//! macOS 소리 — 뜨기는 ScreenCaptureKit 의 시스템 소리(이 앱이 튼 소리는 뺀다), 틀기는 기본 출력 AudioUnit.

use std::ffi::c_void;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use kasa_remote::screen_stream::audio::{Jitter, CHANNELS, RATE};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, sel, AllocAnyThread, DefinedClass};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{SCContentFilter, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType};

use super::audio::ToWire;

type Sink = Box<dyn FnMut(f64, &[i16]) + Send>;

struct Feed {
    sink: Sink,
    /// 덩이 표본율과 그 율에 맞춘 바꾸개 — 율이 바뀌면 새로 만든다.
    wire: Option<(u32, ToWire)>,
    frames: Vec<[f32; 2]>,
}

struct Ivars {
    feed: Mutex<Feed>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "KasaScreenAudioOutput"]
    #[ivars = Ivars]
    struct AudioOutput;

    unsafe impl NSObjectProtocol for AudioOutput {}

    unsafe impl SCStreamOutput for AudioOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Audio {
                return;
            }
            let mut feed = self.ivars().feed.lock().unwrap_or_else(|e| e.into_inner());
            let feed = &mut *feed;
            let Some(rate) = (unsafe { read_pcm(sample, &mut feed.frames) }) else { return };
            if feed.wire.as_ref().is_none_or(|(r, _)| *r != rate) {
                feed.wire = Some((rate, ToWire::new(rate)));
            }
            let t = unsafe { sample.presentation_time_stamp().seconds() };
            let Some((_, wire)) = feed.wire.as_mut() else { return };
            let pcm = wire.convert(&feed.frames);
            if !pcm.is_empty() {
                (feed.sink)(t, pcm);
            }
        }
    }
);

impl AudioOutput {
    fn new(sink: Sink) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { feed: Mutex::new(Feed { sink, wire: None, frames: Vec::new() }) });
        unsafe { msg_send![super(this), init] }
    }
}

#[repr(C)]
struct AudioBuffer {
    channels: u32,
    size: u32,
    data: *mut c_void,
}

#[repr(C)]
struct BufferList<const N: usize> {
    count: u32,
    buffers: [AudioBuffer; N],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Asbd {
    rate: f64,
    format: u32,
    flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels: u32,
    bits: u32,
    reserved: u32,
}

const FORMAT_FLAG_FLOAT: u32 = 1;
const FORMAT_FLAG_SIGNED: u32 = 1 << 2;
const FORMAT_FLAG_PACKED: u32 = 1 << 3;
const FORMAT_FLAG_NON_INTERLEAVED: u32 = 1 << 5;
const ASSURE_16_BYTE_ALIGNMENT: u32 = 1;

extern "C" {
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sbuf: *const CMSampleBuffer,
        size_needed: *mut usize,
        list: *mut c_void,
        list_size: usize,
        structure_allocator: *const c_void,
        memory_allocator: *const c_void,
        flags: u32,
        block: *mut *mut c_void,
    ) -> i32;
    fn CMSampleBufferGetFormatDescription(sbuf: *const CMSampleBuffer) -> *const c_void;
    fn CMAudioFormatDescriptionGetStreamBasicDescription(desc: *const c_void) -> *const Asbd;
    fn CFRelease(cf: *const c_void);
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGMainDisplayID() -> u32;
}

/// SCK 소리 덩이(f32, 보통 채널마다 판이 따로)를 왼쪽·오른쪽 장들로. 표본율을 돌려준다.
unsafe fn read_pcm(sample: &CMSampleBuffer, out: &mut Vec<[f32; 2]>) -> Option<u32> {
    let desc = unsafe { CMSampleBufferGetFormatDescription(sample) };
    if desc.is_null() {
        return None;
    }
    let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(desc).as_ref() }.copied()?;
    if asbd.flags & FORMAT_FLAG_FLOAT == 0 || asbd.bits != 32 || asbd.channels == 0 || asbd.rate < 1.0 {
        return None;
    }
    // 목록 크기는 꼭 물어 받은 그대로 준다 — 넉넉한 고정 크기(8판)를 주면 kCMSampleBufferError_ArrayTooSmall(-12737)이다.
    let mut need = 0usize;
    let asked = unsafe {
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sample,
            &mut need,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            ASSURE_16_BYTE_ALIGNMENT,
            std::ptr::null_mut(),
        )
    };
    let head = std::mem::offset_of!(BufferList<1>, buffers);
    if asked != 0 || need < size_of::<BufferList<1>>() {
        return None;
    }
    let mut mem = vec![0u64; need.div_ceil(8)];
    let mut block: *mut c_void = std::ptr::null_mut();
    let status = unsafe {
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sample,
            std::ptr::null_mut(),
            mem.as_mut_ptr().cast(),
            need,
            std::ptr::null(),
            std::ptr::null(),
            ASSURE_16_BYTE_ALIGNMENT,
            &mut block,
        )
    };
    if status != 0 {
        return None;
    }
    let count = (unsafe { *mem.as_ptr().cast::<u32>() } as usize).min((need - head) / size_of::<AudioBuffer>());
    let buffers = unsafe { std::slice::from_raw_parts(mem.as_ptr().cast::<u8>().add(head).cast::<AudioBuffer>(), count) };
    let plane = |i: usize| -> &[f32] {
        let b = &buffers[i];
        if b.data.is_null() {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(b.data as *const f32, b.size as usize / 4) }
    };
    out.clear();
    if asbd.flags & FORMAT_FLAG_NON_INTERLEAVED != 0 && count > 0 {
        let l = plane(0);
        let r = if count > 1 { plane(1) } else { l };
        out.extend(l.iter().zip(r).map(|(l, r)| [*l, *r]));
    } else if count > 0 {
        let ch = asbd.channels as usize;
        out.extend(plane(0).chunks_exact(ch).map(|f| [f[0], *f.get(1).unwrap_or(&f[0])]));
    }
    if !block.is_null() {
        unsafe { CFRelease(block) };
    }
    Some(asbd.rate.round() as u32)
}

pub(super) struct Source;

struct Running {
    stream: Retained<SCStream>,
    _output: Retained<AudioOutput>,
}

// SCK 객체는 어느 스레드에서 멈춰도 된다. 소리 덩이는 SCK 자기 큐에서 온다.
unsafe impl Send for Running {}

impl Drop for Running {
    fn drop(&mut self) {
        let (tx, rx) = mpsc::channel::<()>();
        let done = RcBlock::new(move |_err: *mut NSError| {
            let _ = tx.send(());
        });
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&done)) };
        let _ = rx.recv_timeout(Duration::from_secs(3));
    }
}

impl kasa_remote::screen_stream::audio::Source for Source {
    /// 주 화면 필터로 SCK 를 열되 화면은 2×2·초당 1장만 받는다 — 소리만 쓴다. 화면 출력도 붙여 두는 까닭: 소리 출력만
    /// 있으면 SCK 가 화면 장마다 「stream output NOT found」를 로그에 쏟는다.
    fn start(&self, sink: Sink) -> Result<Box<dyn Send>, String> {
        if !unsafe { CGPreflightScreenCaptureAccess() } {
            return Err("화면 기록 권한이 없다(시스템 소리도 같은 권한이다)".into());
        }
        let content = super::capture::shareable_content().map_err(|e| format!("{e:#}"))?;
        let main = unsafe { CGMainDisplayID() };
        let displays = unsafe { content.displays() };
        let display = displays
            .iter()
            .find(|d| unsafe { d.displayID() } == main)
            .or_else(|| displays.firstObject())
            .ok_or("화면이 없다")?;
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new())
        };
        let config = unsafe { SCStreamConfiguration::new() };
        let has_audio: bool = unsafe { msg_send![&*config, respondsToSelector: sel!(setCapturesAudio:)] };
        if !has_audio {
            return Err("이 macOS 판(13 이전)은 화면 뜨기로 소리를 못 받는다".into());
        }
        unsafe {
            config.setWidth(2);
            config.setHeight(2);
            config.setMinimumFrameInterval(CMTime::new(1, 1));
            config.setQueueDepth(3);
            config.setShowsCursor(false);
            config.setCapturesAudio(true);
            config.setSampleRate(RATE as isize);
            config.setChannelCount(CHANNELS as isize);
            config.setExcludesCurrentProcessAudio(true);
        }
        let output = AudioOutput::new(sink);
        let stream = unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None) };
        let queue = DispatchQueue::new("kasa.screen-stream.audio", None);
        for kind in [SCStreamOutputType::Audio, SCStreamOutputType::Screen] {
            unsafe {
                stream
                    .addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), kind, Some(&queue))
                    .map_err(|e| format!("addStreamOutput: {}", e.localizedDescription()))?;
            }
        }
        let (tx, rx) = mpsc::channel();
        let done = RcBlock::new(move |err: *mut NSError| {
            let r = if err.is_null() { Ok(()) } else { Err(unsafe { (*err).localizedDescription() }.to_string()) };
            let _ = tx.send(r);
        });
        unsafe { stream.startCaptureWithCompletionHandler(Some(&done)) };
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Box::new(Running { stream, _output: output })),
            Ok(Err(e)) => Err(format!("소리 뜨기 시작 실패: {e}")),
            Err(_) => Err("소리 뜨기 시작이 10초 안에 끝나지 않았다".into()),
        }
    }
}

// ---------------------------------------------------------------- 틀기

#[repr(C)]
struct ComponentDesc {
    kind: u32,
    sub_kind: u32,
    maker: u32,
    flags: u32,
    flags_mask: u32,
}

type RenderFn = unsafe extern "C" fn(*mut c_void, *mut u32, *const c_void, u32, u32, *mut BufferList<1>) -> i32;

#[repr(C)]
struct RenderCallback {
    render: RenderFn,
    refcon: *mut c_void,
}

#[link(name = "AudioToolbox", kind = "framework")]
extern "C" {
    fn AudioComponentFindNext(after: *mut c_void, desc: *const ComponentDesc) -> *mut c_void;
    fn AudioComponentInstanceNew(component: *mut c_void, out: *mut *mut c_void) -> i32;
    fn AudioComponentInstanceDispose(unit: *mut c_void) -> i32;
    fn AudioUnitSetProperty(unit: *mut c_void, id: u32, scope: u32, element: u32, data: *const c_void, size: u32) -> i32;
    fn AudioUnitInitialize(unit: *mut c_void) -> i32;
    fn AudioUnitUninitialize(unit: *mut c_void) -> i32;
    fn AudioOutputUnitStart(unit: *mut c_void) -> i32;
    fn AudioOutputUnitStop(unit: *mut c_void) -> i32;
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

const PROPERTY_STREAM_FORMAT: u32 = 8;
const PROPERTY_RENDER_CALLBACK: u32 = 23;
const SCOPE_INPUT: u32 = 1;
const RENDER_OUTPUT_IS_SILENCE: u32 = 1 << 4;

struct Ctx {
    jitter: Arc<Mutex<Jitter>>,
    silent: bool,
}

/// 기본 출력 장치로 튼다 — 시스템 설정의 출력 장치를 따라가고, 표본율·채널 바꾸기는 AudioUnit 이 한다(장치 설정은 안 건드린다).
pub(super) struct Output {
    unit: *mut c_void,
    ctx: *mut Ctx,
}

// AudioUnit 은 어느 스레드에서 멈춰도 된다. 그리기는 장치 스레드가 `render` 로 한다.
unsafe impl Send for Output {}

impl Output {
    pub(super) fn start(jitter: Arc<Mutex<Jitter>>, silent: bool) -> Result<Self, String> {
        let desc = ComponentDesc { kind: fourcc(b"auou"), sub_kind: fourcc(b"def "), maker: fourcc(b"appl"), flags: 0, flags_mask: 0 };
        let component = unsafe { AudioComponentFindNext(std::ptr::null_mut(), &desc) };
        if component.is_null() {
            return Err("기본 출력 AudioUnit 이 없다".into());
        }
        let mut unit = std::ptr::null_mut();
        check(unsafe { AudioComponentInstanceNew(component, &mut unit) }, "AudioUnit 만들기")?;
        let out = Self { unit, ctx: Box::into_raw(Box::new(Ctx { jitter, silent })) };
        let format = Asbd {
            rate: f64::from(RATE),
            format: fourcc(b"lpcm"),
            flags: FORMAT_FLAG_SIGNED | FORMAT_FLAG_PACKED,
            bytes_per_packet: (CHANNELS * 2) as u32,
            frames_per_packet: 1,
            bytes_per_frame: (CHANNELS * 2) as u32,
            channels: CHANNELS as u32,
            bits: 16,
            reserved: 0,
        };
        let callback = RenderCallback { render, refcon: out.ctx.cast() };
        unsafe {
            check(
                AudioUnitSetProperty(unit, PROPERTY_STREAM_FORMAT, SCOPE_INPUT, 0, (&format as *const Asbd).cast(), size_of::<Asbd>() as u32),
                "형식",
            )?;
            check(
                AudioUnitSetProperty(
                    unit,
                    PROPERTY_RENDER_CALLBACK,
                    SCOPE_INPUT,
                    0,
                    (&callback as *const RenderCallback).cast(),
                    size_of::<RenderCallback>() as u32,
                ),
                "콜백",
            )?;
            check(AudioUnitInitialize(unit), "초기화")?;
            check(AudioOutputUnitStart(unit), "시작")?;
        }
        Ok(out)
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        unsafe {
            AudioOutputUnitStop(self.unit);
            AudioUnitUninitialize(self.unit);
            AudioComponentInstanceDispose(self.unit);
            drop(Box::from_raw(self.ctx));
        }
    }
}

fn check(status: i32, what: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("{what}: OSStatus {status}"))
    }
}

/// 장치 스레드가 부른다 — 지터 버퍼에서 그만큼 꺼낸다(모으는 중이거나 비면 소리 없음).
unsafe extern "C" fn render(refcon: *mut c_void, flags: *mut u32, _ts: *const c_void, _bus: u32, _frames: u32, io: *mut BufferList<1>) -> i32 {
    let (Some(ctx), Some(list)) = (unsafe { (refcon as *const Ctx).as_ref() }, unsafe { io.as_mut() }) else { return 0 };
    if list.count == 0 || list.buffers[0].data.is_null() {
        return 0;
    }
    let b = &mut list.buffers[0];
    let out = unsafe { std::slice::from_raw_parts_mut(b.data as *mut i16, b.size as usize / 2) };
    let sounded = ctx.jitter.lock().unwrap_or_else(|e| e.into_inner()).pull(out);
    if ctx.silent || !sounded {
        out.fill(0);
        if let Some(f) = unsafe { flags.as_mut() } {
            *f |= RENDER_OUTPUT_IS_SILENCE;
        }
    }
    0
}
