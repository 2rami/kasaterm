//! Windows 보내기(`docs/screen-stream.md` S3) — DXGI 데스크톱 복제로 주 화면을 GPU 텍스처째 뜨고 Media Foundation 하드웨어
//! 인코더(NVENC·Quick Sync·AMF 가 MFT 로 낸다)로 압축한다. 인코더는 Annex-B 로 내므로 받는 쪽 꼴(길이 머리 NAL + 파라미터 셋)로
//! 바꿔 싣는다(`annexb`).
//!
//! 인코더는 비동기 MFT 다. 「입력 줘」를 받고 화면 장을 기다리며 막혀 있으면 앞 장의 「출력 있음」 처리가 밀려(화면이 멈추면
//! 영영) 지연이 늘어 사건 대기는 따로 스레드에 두고 장·사건을 한 채널로 받는다.
//!
//! 화면 도우미 서비스(`win_agent.rs`)가 떠 있으면 뜨기·조작은 그쪽(SYSTEM)이 하고 이 프로세스는 파이프로 장을 받는다 — 그래야
//! UAC·잠금 화면(보안 데스크톱)까지 보이고 눌린다. 없으면 여기서 뜬다.

use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kasa_remote::screen_mirror::{Denied, Input};
use kasa_remote::screen_stream::{Capture, Config, Frame, Session};
use serde_json::{json, Value};
use windows::core::{Interface, GUID};
use windows::Win32::Foundation::{E_ACCESSDENIED, HMODULE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIResource,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_INVALID_CALL, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, SetThreadDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, HDESK,
    UOI_NAME,
};
use windows::Win32::System::Variant::VARIANT;

use super::annexb::{self, Nal};

/// 보낸 장을 인코더가 아직 읽을 수 있어 고리로 돌려 쓴다.
const RING: usize = 6;
const HNS_PER_SEC: f64 = 10_000_000.0;

pub(crate) struct Host;

pub(crate) fn install() {
    kasa_remote::screen_stream::install(Arc::new(Host));
}

/// 호스트 시계(초) — 부팅부터의 QPC. 화면 도우미(다른 프로세스)가 찍은 장의 시각과 이 앱의 시계가 같아야 해서
/// 프로세스마다 기점이 다른 `Instant` 를 쓰지 않는다.
pub(super) fn now() -> f64 {
    let (mut count, mut freq) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut count);
        let _ = QueryPerformanceFrequency(&mut freq);
    }
    count as f64 / freq.max(1) as f64
}

fn desktop_name(desk: HDESK) -> String {
    let mut buf = [0u16; 64];
    let ok = unsafe { GetUserObjectInformationW(windows::Win32::Foundation::HANDLE(desk.0), UOI_NAME, Some(buf.as_mut_ptr().cast()), 128, None) };
    ok.map(|()| String::from_utf16_lossy(&buf).trim_end_matches('\0').to_string()).unwrap_or_default()
}

/// 이 스레드를 지금 입력을 받는 데스크톱(평소 Default, UAC·잠금 화면이면 Winlogon)으로 옮긴다. Winlogon 은 SYSTEM 만 열 수
/// 있어 사용자 앱에서는 실패하고 그대로 둔다. 창·훅이 없는 스레드에서만 옮겨진다.
pub(super) fn follow_input_desktop(current: &mut Option<HDESK>) {
    const GENERIC_ALL: u32 = 0x1000_0000;
    let Ok(desk) = (unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(GENERIC_ALL)) }) else { return };
    if unsafe { SetThreadDesktop(desk) }.is_ok() {
        if let Some(old) = current.replace(desk) {
            let _ = unsafe { CloseDesktop(old) };
        }
    } else {
        let _ = unsafe { CloseDesktop(desk) };
    }
}

pub(super) fn com() {
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

pub(super) fn startup() -> Result<(), String> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.message()))
        .clone()
}

struct SendCom<T>(T);
// SAFETY: MF·D3D11 객체는 자유 스레드(MTA)이고 장치는 멀티스레드 보호를 켰다.
unsafe impl<T> Send for SendCom<T> {}
unsafe impl<T> Sync for SendCom<T> {}

/// 주 화면이 붙은 어댑터의 장치와 그 화면(복제는 뜨기 스레드가 그 스레드의 데스크톱에서 연다).
fn open_output() -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext, IDXGIOutput1, u32, u32)> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let adapter: IDXGIAdapter1 = unsafe { factory.EnumAdapters1(0)? };
    let output: IDXGIOutput1 = unsafe { adapter.EnumOutputs(0)? }.cast()?;
    let (mut device, mut context) = (None, None);
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?
    };
    let device = device.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
    let context = context.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
    if let Ok(m) = device.cast::<ID3D11Multithread>() {
        let _ = unsafe { m.SetMultithreadProtected(true) };
    }
    let r = unsafe { output.GetDesc()? }.DesktopCoordinates;
    Ok((device, context, output, (r.right - r.left) as u32, (r.bottom - r.top) as u32))
}

fn texture(device: &ID3D11Device, w: u32, h: u32) -> windows::core::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut t = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut t))? };
    t.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))
}

/// 첫 하드웨어 인코더(정렬은 MF 가 한다 — 보통 그 화면의 GPU 것).
fn hardware_encoder(subtype: GUID) -> Option<IMFActivate> {
    first_mft(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, None, Some(subtype))
}

/// 그 갈래에서 MF 가 앞에 세운 변환 하나. `input`·`output` 은 받는·내는 영상 꼴.
pub(super) fn first_mft(category: GUID, flags: MFT_ENUM_FLAG, input: Option<GUID>, output: Option<GUID>) -> Option<IMFActivate> {
    let info = |subtype| MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype };
    let (input, output) = (input.map(info), output.map(info));
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnumEx(category, flags, input.as_ref().map(|i| i as *const _), output.as_ref().map(|o| o as *const _), &mut list, &mut count).ok()?
    };
    if list.is_null() {
        return None;
    }
    let items = unsafe { std::slice::from_raw_parts_mut(list, count as usize) };
    let first = items.iter_mut().find_map(Option::take);
    for rest in items.iter_mut() {
        drop(rest.take());
    }
    unsafe { CoTaskMemFree(Some(list as _)) };
    first
}

fn media_type(subtype: GUID, w: u32, h: u32, fps: u32, bitrate: u32) -> windows::core::Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(w) << 32) | u64::from(h))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        Ok(t)
    }
}

/// 비동기 하드웨어 인코더 하나 — 입력은 BGRA(ARGB32) D3D11 텍스처.
struct Encoder {
    mft: IMFTransform,
    codec: ICodecAPI,
    _manager: IMFDXGIDeviceManager,
    rejected: Vec<String>,
}

impl Encoder {
    fn new(device: &ID3D11Device, kind: Nal, w: u32, h: u32, fps: u32, bitrate: u32) -> windows::core::Result<Self> {
        let subtype = if kind == Nal::Hevc { MFVideoFormat_HEVC } else { MFVideoFormat_H264 };
        let activate = hardware_encoder(subtype).ok_or_else(|| windows::core::Error::from_hresult(MF_E_TOPO_CODEC_NOT_FOUND))?;
        let mft: IMFTransform = unsafe { activate.ActivateObject()? };
        unsafe { mft.GetAttributes()?.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)? };
        let (mut token, mut manager) = (0u32, None);
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager)? };
        let manager = manager.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
        unsafe { manager.ResetDevice(device, token)? };
        unsafe { mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)? };
        let codec: ICodecAPI = mft.cast()?;
        let mut rejected = Vec::new();
        let mut set = |api: &GUID, v: VARIANT, label: &str| {
            if unsafe { codec.SetValue(api, &v) }.is_err() {
                rejected.push(label.to_string());
            }
        };
        set(&CODECAPI_AVLowLatencyMode, VARIANT::from(true), "LowLatencyMode");
        set(&CODECAPI_AVEncCommonRateControlMode, VARIANT::from(eAVEncCommonRateControlMode_LowDelayVBR.0 as u32), "LowDelayVBR");
        set(&CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(bitrate), "MeanBitRate");
        set(&CODECAPI_AVEncMPVDefaultBPictureCount, VARIANT::from(0u32), "BPictureCount");
        // 키프레임은 받는 쪽이 청할 때만 — 주기 키프레임은 큰 장이 주기적으로 지연을 튀게 한다.
        set(&CODECAPI_AVEncMPVGOPSize, VARIANT::from(0u32), "GOPSize");
        unsafe { mft.SetOutputType(0, &media_type(subtype, w, h, fps, bitrate)?, 0)? };
        let input = (0..)
            .map_while(|i| unsafe { mft.GetInputAvailableType(0, i) }.ok())
            .find(|t| unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.ok() == Some(MFVideoFormat_ARGB32))
            .ok_or_else(|| windows::core::Error::from_hresult(MF_E_INVALIDMEDIATYPE))?;
        unsafe {
            input.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(w) << 32) | u64::from(h))?;
            input.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)?;
            mft.SetInputType(0, &input, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(Self { mft, codec, _manager: manager, rejected })
    }

    fn force_keyframe(&self) {
        let _ = unsafe { self.codec.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32)) };
    }

    fn put(&self, tex: &ID3D11Texture2D, pts: i64, duration: i64) -> windows::core::Result<()> {
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, tex, 0, false)?;
            if let Ok(b2) = buffer.cast::<IMF2DBuffer>() {
                buffer.SetCurrentLength(b2.GetContiguousLength()?)?;
            }
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(duration)?;
            self.mft.ProcessInput(0, &sample, 0)
        }
    }

    /// 다 된 장 하나(Annex-B 바이트와 시각).
    fn take(&self) -> Option<(Vec<u8>, i64)> {
        unsafe {
            let mut out = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(None),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let got = self.mft.ProcessOutput(0, &mut out, &mut status);
            let sample = ManuallyDrop::take(&mut out[0].pSample);
            ManuallyDrop::drop(&mut out[0].pEvents);
            got.ok()?;
            let sample = sample?;
            let time = sample.GetSampleTime().unwrap_or(0);
            let buf = sample.ConvertToContiguousBuffer().ok()?;
            let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
            buf.Lock(&mut p, None, Some(&mut len)).ok()?;
            let data = std::slice::from_raw_parts(p, len as usize).to_vec();
            let _ = buf.Unlock();
            Some((data, time))
        }
    }

    fn shutdown(&self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(s) = self.mft.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
        }
    }
}

enum Msg {
    NeedInput,
    HaveOutput,
    Frame(SendCom<ID3D11Texture2D>, f64, u64),
    Keyframe,
    Stop,
}

/// 주 화면 복제 하나를 보기 여럿(맥 칸·폰)이 나눠 쓴다 — 한 프로세스는 같은 출력을 두 번 복제하지 못한다(두 번째
/// `DuplicateOutput` 이 E_INVALIDARG). 마지막 보기가 떠나면 뜨기 스레드와 복제가 닫힌다.
struct Desktop {
    device: ID3D11Device,
    w: u32,
    h: u32,
    feed: Arc<Feed>,
    thread: Option<JoinHandle<()>>,
}

struct Feed {
    stop: AtomicBool,
    /// 복제가 끊겼다(잠금 화면·해상도 바꿈) — 다음 보기는 새로 연다.
    dead: AtomicBool,
    subs: Mutex<Vec<Sub>>,
}

/// 보기 하나의 박자 — 장은 고리를 같이 쓰고 fps·다듬기만 보기마다 센다.
struct Sub {
    id: u64,
    tx: mpsc::Sender<Msg>,
    period: Duration,
    refine: u32,
    refine_left: u32,
    last_sent: Instant,
}

impl Desktop {
    fn shared() -> windows::core::Result<Arc<Desktop>> {
        static CURRENT: OnceLock<Mutex<Weak<Desktop>>> = OnceLock::new();
        let mut current = CURRENT.get_or_init(|| Mutex::new(Weak::new())).lock().unwrap_or_else(|e| e.into_inner());
        if let Some(d) = current.upgrade().filter(|d| !d.feed.dead.load(Ordering::Acquire)) {
            return Ok(d);
        }
        let d = Arc::new(Self::open()?);
        *current = Arc::downgrade(&d);
        Ok(d)
    }

    fn open() -> windows::core::Result<Self> {
        let (device, context, output, w, h) = open_output()?;
        let ring: Vec<ID3D11Texture2D> = (0..RING).map(|_| texture(&device, w, h)).collect::<windows::core::Result<_>>()?;
        let feed = Arc::new(Feed { stop: AtomicBool::new(false), dead: AtomicBool::new(false), subs: Mutex::new(Vec::new()) });
        let cap: Parts = SendCom((output, device.clone(), context, ring));
        let thread_feed = feed.clone();
        let thread = std::thread::Builder::new()
            .name("screen-stream-capture".into())
            .spawn(move || {
                let cap = cap;
                capture_loop(&cap, (w, h), &thread_feed);
                drop(cap);
                thread_feed.dead.store(true, Ordering::Release);
                for sub in thread_feed.subs.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                    let _ = sub.tx.send(Msg::Stop);
                }
            })
            .map_err(|_| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
        Ok(Self { device, w, h, feed, thread: Some(thread) })
    }

    fn subscribe(&self, tx: mpsc::Sender<Msg>, fps: u32, refine: u32) -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let period = Duration::from_secs_f64(1.0 / f64::from(fps));
        // 화면이 멈춰 있어도 새 보기가 첫 장을 바로 받게 — 다듬기 한 번이 마지막 장을 보낸다.
        let last_sent = Instant::now().checked_sub(period).unwrap_or_else(Instant::now);
        let sub = Sub { id, tx, period, refine, refine_left: refine.max(1), last_sent };
        self.feed.subs.lock().unwrap_or_else(|e| e.into_inner()).push(sub);
        id
    }

    fn unsubscribe(&self, id: u64) {
        self.feed.subs.lock().unwrap_or_else(|e| e.into_inner()).retain(|s| s.id != id);
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.feed.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 데스크톱이 바뀌어도 이어지는 뜨기 상태.
#[derive(Default)]
struct Progress {
    n: usize,
    cap_id: u64,
    last: Option<ID3D11Texture2D>,
}

enum Turn {
    /// 입력 데스크톱이 바뀌었다(UAC·잠금 화면) — 새 스레드로 그 데스크톱에서 다시.
    Again,
    Done,
}

type Parts = SendCom<(IDXGIOutput1, ID3D11Device, ID3D11DeviceContext, Vec<ID3D11Texture2D>)>;

/// 데스크톱마다 새 스레드로 뜬다. 스레드는 창·훅이 생기면 다른 데스크톱으로 못 옮기는데(`SetThreadDesktop`), DXGI 를 한
/// 번 쓴 스레드는 Winlogon 으로 못 옮겨 UAC 동안 장이 하나도 안 나왔다(2026-11-16 실측).
fn capture_loop(parts: &Parts, size: (u32, u32), feed: &Feed) {
    let mut progress = SendCom(Progress::default());
    while !feed.stop.load(Ordering::Relaxed) {
        let turn = std::thread::scope(|s| {
            s.spawn(|| capture_desktop(parts, size, feed, &mut progress.0)).join().unwrap_or(Turn::Done)
        });
        if matches!(turn, Turn::Done) {
            return;
        }
    }
}

/// 이 스레드를 입력 데스크톱에 두고 복제를 열어, 바뀐 장만 고리의 다음 텍스처로 복사해 보기마다 fps 에 맞춰 넘긴다. 멈추면
/// 마지막 장을 보기마다 `refine` 번 더(다듬기). 보안 데스크톱에 못 들어가면(SYSTEM 이 아닌 앱) 돌아올 때까지 다시 묻는다.
fn capture_desktop(parts: &Parts, size: (u32, u32), feed: &Feed, p: &mut Progress) -> Turn {
    let (output, device, context, ring) = &parts.0;
    com();
    let mut desk = None;
    follow_input_desktop(&mut desk);
    let close = |desk: Option<HDESK>| {
        if let Some(d) = desk {
            let _ = unsafe { CloseDesktop(d) };
        }
    };
    let dup = match unsafe { output.DuplicateOutput(device) } {
        Ok(dup) => dup,
        Err(e) => {
            if e.code() != E_ACCESSDENIED {
                eprintln!("[screen-stream] 화면 복제를 못 연다: {e}");
            }
            std::thread::sleep(Duration::from_millis(if e.code() == E_ACCESSDENIED { 100 } else { 250 }));
            close(desk);
            return Turn::Again;
        }
    };
    if p.n > 0 {
        eprintln!("[screen-stream] 입력 데스크톱 {} 에서 다시 뜬다", desk.map(desktop_name).unwrap_or_default());
    }
    let mode = unsafe { dup.GetDesc() }.ModeDesc;
    if (mode.Width, mode.Height) != size {
        // 해상도가 바뀌었다 — 이 복제는 끝이고 보는 쪽이 새 크기로 다시 붙는다.
        close(desk);
        return Turn::Done;
    }
    // 새 복제의 첫 장은 그 데스크톱 전체 그림인데 멈춘 화면(UAC 창)이면 마우스 변화만 있는 장(`LastPresentTime` 0)으로
    // 온다. 그것을 버리면 다음 변화가 없어 보는 쪽이 그 화면을 영영 못 본다.
    let mut first = true;
    let turn = loop {
        if feed.stop.load(Ordering::Relaxed) {
            break Turn::Done;
        }
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        // 새 장을 기다리는 동안 장치 잠금을 쥐면 인코더가 그 사이 장치를 못 써 압축이 다음 장까지 밀린다
        // (기다림 16ms 로 캡처→압축 끝 p50 58ms, 0 으로 묻고 밖에서 쉬면 7ms — S3 실측).
        match unsafe { dup.AcquireNextFrame(0, &mut info, &mut res) } {
            Ok(()) => {
                let t = now();
                if info.LastPresentTime != 0 || std::mem::take(&mut first) {
                    if let Some(src) = res.and_then(|r| r.cast::<ID3D11Texture2D>().ok()) {
                        let dst = ring[p.n % ring.len()].clone();
                        unsafe { context.CopyResource(&dst, &src) };
                        p.n += 1;
                        p.cap_id += 1;
                        p.last = Some(dst.clone());
                        for sub in feed.subs.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
                            // 주사율이 fps 보다 높은 모니터(144Hz 등)는 장을 더 준다 — fps 로 묶는다. 이른 장은 쥐어 두고
                            // 다듬기가 한 간격 뒤에 보내 마지막 화면을 놓치지 않는다.
                            if sub.last_sent.elapsed() >= sub.period.mul_f32(0.9) {
                                sub.last_sent = Instant::now();
                                sub.refine_left = sub.refine;
                                let _ = sub.tx.send(Msg::Frame(SendCom(dst.clone()), t, p.cap_id));
                            } else {
                                sub.refine_left = sub.refine.max(1);
                            }
                        }
                    }
                }
                let _ = unsafe { dup.ReleaseFrame() };
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                if let Some(tex) = &p.last {
                    for sub in feed.subs.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
                        if sub.refine_left > 0 && sub.last_sent.elapsed() >= sub.period {
                            sub.refine_left -= 1;
                            sub.last_sent = Instant::now();
                            let _ = sub.tx.send(Msg::Frame(SendCom(tex.clone()), now(), p.cap_id));
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => break Turn::Again,
            // UAC 데스크톱에서 Default 로 돌아올 때 ACCESS_LOST 대신 이것이 오기도 한다(2026-11-16 실측).
            Err(e) if e.code() == DXGI_ERROR_INVALID_CALL => {
                std::thread::sleep(Duration::from_millis(20));
                break Turn::Again;
            }
            Err(e) => {
                eprintln!("[screen-stream] 화면 복제 끊김: {e}");
                break Turn::Done;
            }
        }
    };
    drop(dup);
    close(desk);
    turn
}

struct WinSession {
    tx: Mutex<mpsc::Sender<Msg>>,
    desktop: Option<Arc<Desktop>>,
    sub: u64,
    threads: Vec<JoinHandle<()>>,
    info: Value,
    display: u32,
}

impl Session for WinSession {
    fn keyframe(&self) {
        let _ = self.tx.lock().unwrap_or_else(|e| e.into_inner()).send(Msg::Keyframe);
    }

    fn info(&self) -> Value {
        self.info.clone()
    }

    fn display(&self) -> u32 {
        self.display
    }
}

impl Drop for WinSession {
    fn drop(&mut self) {
        if let Some(d) = &self.desktop {
            d.unsubscribe(self.sub);
        }
        let _ = self.tx.lock().unwrap_or_else(|e| e.into_inner()).send(Msg::Stop);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        self.desktop.take();
    }
}

/// 조작은 화면 거울과 같은 화면 번호(EnumDisplayMonitors 차례)로 넣는다. DXGI 첫 출력이 주 화면이다.
fn main_display() -> u32 {
    kasa_remote::screen_mirror::installed()
        .and_then(|s| s.displays().into_iter().find(|d| d.main).map(|d| d.id))
        .unwrap_or(0)
}

impl Capture for Host {
    fn start(&self, config: &Config, out: Box<dyn Fn(Frame) + Send + Sync>) -> Result<Box<dyn Session>, Denied> {
        let display = main_display();
        match super::win_agent::connect() {
            Some(pipe) => super::win_agent::start(pipe, config, out, display),
            None => start_local(config, out, display),
        }
    }

    fn request_permission(&self) {}

    fn input(&self, display: u32, input: &Input) -> Result<(), Denied> {
        if super::win_agent::input(display, input) {
            return Ok(());
        }
        crate::screen_input::put(display, input)
    }

    fn request_input_permission(&self) {}

    fn now(&self) -> f64 {
        now()
    }
}

/// 이 프로세스에서 뜨고 압축한다 — 도우미가 없을 때 앱이, 그리고 도우미 자신이 쓴다.
pub(super) fn start_local(config: &Config, out: Box<dyn Fn(Frame) + Send + Sync>, display: u32) -> Result<Box<dyn Session>, Denied> {
    com();
    startup().map_err(|e| {
        eprintln!("[screen-stream] Media Foundation 을 못 연다: {e}");
        Denied::Unavailable
    })?;
    let desktop = Desktop::shared().map_err(|e| {
        eprintln!("[screen-stream] 화면 복제를 못 연다: {e}");
        Denied::Unavailable
    })?;
    let (device, w, h) = (&desktop.device, desktop.w, desktop.h);
    let want = if config.codec == "h264" { Nal::H264 } else { Nal::Hevc };
    let fps = config.fps.max(1);
    let unavailable = |e: windows::core::Error| {
        eprintln!("[screen-stream] 하드웨어 인코더를 못 연다: {e}");
        Denied::Unavailable
    };
    // HEVC 인코더가 없는 GPU 면 H.264 로 — 받는 쪽은 파라미터 셋 수로 코덱을 가른다.
    let (encoder, kind) = match Encoder::new(device, want, w, h, fps, config.bitrate) {
        Ok(enc) => (enc, want),
        Err(e) if want == Nal::Hevc => {
            eprintln!("[screen-stream] HEVC 인코더를 못 연다({e}) — H.264 로");
            (Encoder::new(device, Nal::H264, w, h, fps, config.bitrate).map_err(unavailable)?, Nal::H264)
        }
        Err(e) => return Err(unavailable(e)),
    };
    let info = json!({
        "width": w, "height": h, "codec": if kind == Nal::Hevc { "hevc" } else { "h264" }, "fps": fps,
        "bitrate": config.bitrate, "refine": config.refine, "rejected": encoder.rejected,
    });
    let (tx, rx) = mpsc::channel::<Msg>();
    let events: IMFMediaEventGenerator = encoder.mft.cast().map_err(|_| Denied::Unavailable)?;
    let mut threads = Vec::new();
    // 사건 대기 — 인코더를 닫으면 GetEvent 가 오류로 돌아와 끝난다.
    {
        let (events, tx) = (SendCom(events), tx.clone());
        threads.push(std::thread::spawn(move || {
            let events = events;
            com();
            while let Ok(ev) = unsafe { events.0.GetEvent(MF_EVENT_FLAG_NONE) } {
                let kind = unsafe { ev.GetType() }.unwrap_or(0) as i32;
                let msg = if kind == METransformNeedInput.0 {
                    Msg::NeedInput
                } else if kind == METransformHaveOutput.0 {
                    Msg::HaveOutput
                } else {
                    continue;
                };
                if tx.send(msg).is_err() {
                    return;
                }
            }
        }));
    }
    // 압축 — 입력 줄 몫(need)과 가장 새 장 하나만 쥔다.
    {
        let encoder = SendCom(encoder);
        threads.push(std::thread::spawn(move || {
            let encoder = encoder;
            let enc = &encoder.0;
            com();
            let (mut need, mut pending, mut pts, mut seq) = (0u32, None, 0i64, 0u32);
            let mut meta: HashMap<i64, (f64, u64)> = HashMap::new();
            let duration = (HNS_PER_SEC / f64::from(fps)) as i64;
            let mut force = true;
            while let Ok(msg) = rx.recv() {
                match msg {
                    Msg::NeedInput => need += 1,
                    Msg::Frame(tex, t, cap) => pending = Some((tex, t, cap)),
                    Msg::Keyframe => force = true,
                    Msg::HaveOutput => {
                        if let Some((bytes, time)) = enc.take() {
                            let (t_cap, cap_id) = meta.remove(&time).unwrap_or((now(), 0));
                            let (param_sets, data, key) = annexb::split(&bytes, kind);
                            seq += 1;
                            out(Frame { seq, cap_id, key, t_cap, t_enc: now(), width: w, height: h, param_sets, data });
                        }
                    }
                    Msg::Stop => break,
                }
                if need > 0 {
                    if let Some((tex, t, cap)) = pending.take() {
                        if std::mem::take(&mut force) {
                            enc.force_keyframe();
                        }
                        if enc.put(&tex.0, pts, duration).is_ok() {
                            meta.insert(pts, (t, cap));
                            pts += duration;
                            need -= 1;
                        }
                    }
                }
            }
            enc.shutdown();
        }));
    }
    // 인코더 스레드가 받을 채비를 한 뒤에 장을 받기 시작한다.
    let sub = desktop.subscribe(tx.clone(), fps, config.refine);
    Ok(Box::new(WinSession { tx: Mutex::new(tx), desktop: Some(desktop), sub, threads, info, display }))
}
