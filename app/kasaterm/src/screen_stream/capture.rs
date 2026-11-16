//! ScreenCaptureKit 으로 주 화면을 받는다 — 원래 픽셀 크기에 배율을 곱해 GPU 에서 줄인다.

use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCCaptureResolutionType, SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType,
};

/// 화면에 새로 그려진 한 장.
pub struct Frame {
    pub pixels: CFRetained<CVPixelBuffer>,
    /// 화면에 그려진 시각(호스트 시계 초).
    pub t_cap: f64,
}

type Sink = Box<dyn Fn(Frame) + Send + Sync>;

struct Ivars {
    sink: Sink,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "KasaScreenStreamOutput"]
    #[ivars = Ivars]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // 바뀐 것이 없는 장(idle)·빈 장은 그림이 없거나 지난 그림이다 — 새로 그려진 장만 넘긴다.
            if !unsafe { is_complete(sample) } {
                return;
            }
            let Some(pixels) = (unsafe { sample.image_buffer() }) else { return };
            let t_cap = unsafe { sample.presentation_time_stamp().seconds() };
            (self.ivars().sink)(Frame { pixels, t_cap });
        }
    }
);

impl Output {
    fn new(sink: Sink) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { sink });
        unsafe { msg_send![super(this), init] }
    }
}

unsafe fn is_complete(sample: &CMSampleBuffer) -> bool {
    let Some(arr) = (unsafe { sample.sample_attachments_array(false) }) else { return false };
    let arr: &CFArray = &arr;
    if arr.count() < 1 {
        return false;
    }
    let dict = unsafe { arr.value_at_index(0) }.cast::<CFDictionary>();
    if dict.is_null() {
        return false;
    }
    let key: &objc2_foundation::NSString = unsafe { SCStreamFrameInfoStatus };
    let v = unsafe { (*dict).value((key as *const objc2_foundation::NSString).cast()) };
    if v.is_null() {
        return false;
    }
    let num = unsafe { &*v.cast::<CFNumber>() };
    num.as_i64() == Some(0)
}

pub struct Capture {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    pub width: usize,
    pub height: usize,
}

// SCK 객체는 어느 스레드에서 멈춰도 된다. 프레임은 SCK 자기 큐에서 온다.
unsafe impl Send for Capture {}

impl Capture {
    pub fn start(fps: u32, pixel_format: u32, scale_by: f64, sink: impl Fn(Frame) + Send + Sync + 'static) -> Result<Self> {
        let content = shareable_content()?;
        let displays = unsafe { content.displays() };
        let display = displays.firstObject().context("화면이 없다")?;
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new())
        };
        let scale = unsafe { SCShareableContent::infoForFilter(&filter).pointPixelScale() } as f64;
        // 인코더는 짝수 크기를 바란다.
        let even = |v: f64| ((v / 2.0).round() as usize) * 2;
        let width = even(unsafe { display.width() } as f64 * scale * scale_by);
        let height = even(unsafe { display.height() } as f64 * scale * scale_by);

        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setWidth(width);
            config.setHeight(height);
            config.setMinimumFrameInterval(CMTime::new(1, fps as i32));
            config.setPixelFormat(pixel_format);
            config.setQueueDepth(6);
            config.setShowsCursor(true);
            config.setCaptureResolution(SCCaptureResolutionType::Best);
        }

        let output = Output::new(Box::new(sink));
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None)
        };
        let queue = DispatchQueue::new("kasa.screen-stream.capture", None);
        unsafe {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|e| anyhow!("addStreamOutput: {}", e.localizedDescription()))?;
        }
        let (tx, rx) = mpsc::channel();
        let done = RcBlock::new(move |err: *mut NSError| {
            let r = if err.is_null() {
                Ok(())
            } else {
                Err(unsafe { (*err).localizedDescription() }.to_string())
            };
            let _ = tx.send(r);
        });
        unsafe { stream.startCaptureWithCompletionHandler(Some(&done)) };
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => bail!("화면 캡처 시작 실패(화면 기록 권한?): {e}"),
            Err(_) => bail!("화면 캡처 시작이 10초 안에 끝나지 않았다"),
        }
        Ok(Self { stream, _output: output, width, height })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let (tx, rx) = mpsc::channel::<()>();
        let done = RcBlock::new(move |_err: *mut NSError| {
            let _ = tx.send(());
        });
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&done)) };
        let _ = rx.recv_timeout(Duration::from_secs(3));
    }
}

fn shareable_content() -> Result<Retained<SCShareableContent>> {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        let r = match unsafe { Retained::retain(content) } {
            Some(c) => Ok(c),
            None if err.is_null() => Err("빈 응답".to_string()),
            None => Err(unsafe { (*err).localizedDescription() }.to_string()),
        };
        let _ = tx.send(SendBox(r));
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(SendBox(Ok(c))) => Ok(c),
        Ok(SendBox(Err(e))) => bail!("화면 목록을 못 받았다(화면 기록 권한?): {e}"),
        Err(_) => bail!("화면 목록이 10초 안에 오지 않았다"),
    }
}

struct SendBox<T>(T);
unsafe impl<T> Send for SendBox<T> {}
