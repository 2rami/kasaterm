use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, BSTR, GUID, HRESULT, HSTRING};
use windows::Win32::Foundation::{HMODULE, RECT, S_OK};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MFMediaEngineClassFactory, IMF2DBuffer, IMFAttributes, IMFDXGIDeviceManager,
    IMFMediaBuffer, IMFMediaEngine, IMFMediaEngineClassFactory, IMFMediaEngineEx,
    IMFMediaEngineNotify, IMFMediaEngineNotify_Impl, IMFMediaType, IMFSourceReader, MFARGB,
    MFCreateAttributes, MFCreateDXGIDeviceManager, MFCreateFile, MFCreateMediaType,
    MFCreateSourceReaderFromByteStream, MFMediaType_Audio, MFMediaType_Video, MFStartup,
    MFSTARTUP_FULL, MFVideoArea, MFVideoFormat_RGB32, MF_ACCESSMODE_READ, MF_E_INVALIDSTREAMNUMBER,
    MF_FILEFLAGS_NONE, MF_MEDIA_ENGINE_CALLBACK, MF_MEDIA_ENGINE_DXGI_MANAGER, MF_MEDIA_ENGINE_ERR,
    MF_MEDIA_ENGINE_ERR_ABORTED, MF_MEDIA_ENGINE_ERR_DECODE, MF_MEDIA_ENGINE_ERR_ENCRYPTED,
    MF_MEDIA_ENGINE_ERR_NETWORK, MF_MEDIA_ENGINE_ERR_NOERROR,
    MF_MEDIA_ENGINE_ERR_SRC_NOT_SUPPORTED, MF_MEDIA_ENGINE_EVENT, MF_MEDIA_ENGINE_EVENT_ERROR,
    MF_MEDIA_ENGINE_EVENT_SEEKED, MF_MEDIA_ENGINE_EVENT_SEEKING, MF_MEDIA_ENGINE_PRELOAD_AUTOMATIC,
    MF_MEDIA_ENGINE_READY_HAVE_FUTURE_DATA, MF_MEDIA_ENGINE_READY_HAVE_METADATA,
    MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MF_MT_VIDEO_ROTATION,
    MF_OPENMODE_FAIL_IF_NOT_EXIST, MF_PD_DURATION, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_MEDIASOURCE, MF_VERSION,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    COINIT_MULTITHREADED,
};

use super::{
    playback_ended, Frame, Orientation, Playback, Probe, POSTER_MAX_BYTES, POSTER_MAX_EDGE,
};

#[path = "windows_stream.rs"]
mod stream;

/// 프레임을 매번 CPU 로 RGBA 변환하므로 큰 원본은 이 긴 변으로 줄여 받는다(옮길 때 GPU 가 줄인다).
const FRAME_MAX_EDGE: u32 = 1920;
/// 깨진 파일·느린 볼륨에서 검사 스레드가 영영 붙잡히지 않게.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// 끝 지점처럼 새 프레임이 끝내 안 나오는 위치 찾기가 렌더 루프를 영영 붙잡지 않게. 스트림은
/// 위치 찾기 자체가 받기를 기다리므로 요청 시각이 아니라 완료를 본 때부터 센다.
const SEEK_GIVE_UP: Duration = Duration::from_secs(5);
/// 정확한 위치 찾기라 새 위치의 프레임은 목표보다 한 프레임 안쪽 앞이다. 24fps 두 장 남짓.
const LANDED_SLACK: f64 = 0.1;
/// Media Foundation 의 시간 단위(100ns)로 1초.
const TICKS: f64 = 10_000_000.0;
/// 프레임 일꾼이 엔진을 깨우는 간격. 재생·위치 찾기 중엔 60fps 원본도 놓치지 않을 만큼 자주.
const TICK_BUSY: Duration = Duration::from_millis(4);
const TICK_IDLE: Duration = Duration::from_millis(16);
/// 재생 중인데 시계가 이만큼 멈춰 있으면 받기를 기다리는 것으로 본다. 엔진의 준비 상태(readyState)는
/// 원본이 끊겨 재생이 멈춘 동안에도 내려가지 않았다(실측).
const STALLED: Duration = Duration::from_millis(250);

/// 재생 엔진이 작업 스레드에서 알리는 것과 프레임 일꾼이 받아 둔 것. 상태 대부분은 엔진에 직접
/// 묻고, 묻기로는 알 수 없는 위치 찾기 완료·실패 까닭과 최신 프레임만 여기 남긴다.
#[derive(Default)]
struct Events {
    /// 위치 찾기 시작·완료 알림에 차례로 매기는 번호와, 마지막 시작·완료의 번호.
    seek_seq: AtomicU64,
    seek_started: AtomicU64,
    seek_done: AtomicU64,
    error: Mutex<Option<(i32, HRESULT)>>,
    /// 엔진이 알림 객체를 놓았다 — 종료가 끝났다는 뜻이다.
    released: AtomicBool,
    /// 아직 안 꺼낸 가장 최근 프레임. 앱이 그리기를 쉬는 동안 온 것은 덮어쓴다.
    latest: Mutex<Option<Captured>>,
    stop: AtomicBool,
}

impl Events {
    fn marks(&self) -> SeekMarks {
        SeekMarks {
            started: self.seek_started.load(Ordering::Acquire),
            done: self.seek_done.load(Ordering::Acquire),
        }
    }
}

/// 알림이 늦게 올 수 있어 개수가 아니라 차례로 판단한다 — 불러올 때 엔진이 스스로 한 찾기의 완료가
/// 막 건 요청 뒤에 도착해도, 그 뒤에 시작된 찾기가 끝나야 끝으로 친다.
#[derive(Clone, Copy)]
struct SeekMarks {
    started: u64,
    done: u64,
}

impl SeekMarks {
    fn finished_after(self, mark: u64) -> bool {
        self.started > mark && self.done > self.started
    }
}

/// 일꾼이 받은 프레임 한 장과, 받을 때 위치 찾기가 어디까지 끝나 있었는지.
struct Captured {
    pts: i64,
    marks: SeekMarks,
    seeking: bool,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[implement(IMFMediaEngineNotify)]
struct Notify(Arc<Events>);

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, param1: usize, param2: u32) -> windows::core::Result<()> {
        let event = MF_MEDIA_ENGINE_EVENT(event as i32);
        if event == MF_MEDIA_ENGINE_EVENT_SEEKING || event == MF_MEDIA_ENGINE_EVENT_SEEKED {
            let seq = self.0.seek_seq.fetch_add(1, Ordering::AcqRel) + 1;
            let slot = if event == MF_MEDIA_ENGINE_EVENT_SEEKING {
                &self.0.seek_started
            } else {
                &self.0.seek_done
            };
            slot.store(seq, Ordering::Release);
        } else if event == MF_MEDIA_ENGINE_EVENT_ERROR {
            *self.0.error.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((param1 as i32, HRESULT(param2 as i32)));
        }
        Ok(())
    }
}

impl Drop for Notify {
    fn drop(&mut self) {
        self.0.released.store(true, Ordering::Release);
    }
}

/// 프레임 서버 모드의 엔진은 앱이 `OnVideoStreamTick` 으로 깨워야 시계가 간다 — 깨우지 않으면
/// 재생 중이어도 위치가 0 에 머문다(실측). 앱의 그리기 주기와 떼어 놓으려고 재생기마다 일꾼
/// 스레드가 엔진을 깨우고, 새 프레임이 오면 GPU 에서 읽어 최신 한 장으로 둔다.
pub struct Player {
    engine: IMFMediaEngine,
    events: Arc<Events>,
    worker: Option<JoinHandle<()>>,
    /// 스트림으로 연 원본. 재생기를 버릴 때 기다리던 읽기를 바로 끊는다.
    source: Option<Arc<stream::Source>>,
    /// 재생 중 마지막으로 위치가 바뀐 때. 시계가 멈춘 시간을 잰다.
    clock: Cell<Option<(f64, Instant)>>,
    /// 메타데이터를 읽은 뒤 한 번 정한다. 그 전엔 프레임을 내지 않는다.
    has_video: Option<bool>,
    serial: u64,
    /// 열기도 0초로 가는 위치 찾기로 친다 — 준비된 뒤 첫 프레임이 나올 때까지 `seeking`.
    pending: Option<PendingSeek>,
    seek_generation: u64,
    /// 메타데이터 전에 받은 위치 찾기. 그 전의 위치 지정은 시작 위치로만 쓰여 완료 알림이 오는지
    /// 정해져 있지 않아, 준비되면 그때 건다.
    deferred: Option<f64>,
}

/// 멈춘 채 위치를 옮기면 새 프레임은 완료 알림 뒤에야 나오고, 그 전엔 옛 위치의 프레임이 한 장
/// 더 나올 수 있다. 그 사이 렌더 루프가 쉬면 막대만 옮겨 가고 그림은 옛 것에 멈춘다.
struct PendingSeek {
    generation: u64,
    target: f64,
    /// 이 요청을 건 때의 알림 번호. 이 뒤에 시작된 찾기가 완료되고 엔진이 찾기를 마쳤으면 끝난 것이다.
    mark: u64,
    settled_at: Cell<Option<Instant>>,
}

impl PendingSeek {
    fn new(generation: u64, target: f64) -> Self {
        Self {
            generation,
            target,
            mark: 0,
            settled_at: Cell::new(None),
        }
    }
}

impl Player {
    pub fn open(path: &Path) -> Result<Self, String> {
        let path = local_file(path)?;
        let player = Self::create()?;
        unsafe {
            let stream = MFCreateFile(
                MF_ACCESSMODE_READ,
                MF_OPENMODE_FAIL_IF_NOT_EXIST,
                MF_FILEFLAGS_NONE,
                &HSTRING::from(path.as_os_str()),
            )
            .map_err(|e| format!("cannot open media file: {}", e.message()))?;
            let engine: IMFMediaEngineEx = player.engine.cast().map_err(failure)?;
            // 이름은 형식 짐작(확장자)에만 쓰인다.
            engine
                .SetSourceFromByteStream(&stream, &BSTR::from(path.to_string_lossy().as_ref()))
                .map_err(failure)?;
        }
        Ok(player)
    }

    pub fn open_url(url: &str) -> Result<Self, String> {
        let mut player = Self::create()?;
        let source = stream::Source::new(url)?;
        player.source = Some(Arc::clone(&source));
        let hint = source.format_hint(url);
        let bytes = stream::ByteStream::new(source)?;
        unsafe {
            let engine: IMFMediaEngineEx = player.engine.cast().map_err(failure)?;
            engine
                .SetSourceFromByteStream(&bytes, &BSTR::from(hint.as_str()))
                .map_err(failure)?;
        }
        Ok(player)
    }

    fn create() -> Result<Self, String> {
        // 앱 메인 스레드는 winit 이 이미 STA 로 열어 두었다 — 같은 방식이면 겹쳐 불러도 된다.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        startup()?;
        let gpu = Gpu::new()?;
        let events = Arc::new(Events::default());
        let engine = unsafe {
            let notify: IMFMediaEngineNotify = Notify(Arc::clone(&events)).into();
            let attributes = attributes(3)?;
            attributes
                .SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)
                .map_err(failure)?;
            attributes
                .SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &gpu.manager)
                .map_err(failure)?;
            attributes
                .SetUINT32(
                    &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                    DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
                )
                .map_err(failure)?;
            let factory: IMFMediaEngineClassFactory =
                CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(failure)?;
            let engine = factory.CreateInstance(0, &attributes).map_err(failure)?;
            // 멈춘 채 연 카드도 첫 장면을 보이려면 메타데이터 너머까지 읽어야 한다.
            engine
                .SetPreload(MF_MEDIA_ENGINE_PRELOAD_AUTOMATIC)
                .map_err(failure)?;
            engine.SetAutoPlay(false).map_err(failure)?;
            engine
        };
        let worker = Worker {
            engine: engine.clone(),
            gpu,
            events: Arc::clone(&events),
        };
        let worker = std::thread::Builder::new()
            .name("media-frames".into())
            .spawn(move || worker.run())
            .map_err(|e| format!("media player failed: {e}"))?;
        Ok(Self {
            engine,
            events,
            worker: Some(worker),
            source: None,
            clock: Cell::new(None),
            has_video: None,
            serial: 0,
            pending: Some(PendingSeek::new(0, 0.0)),
            seek_generation: 0,
            deferred: None,
        })
    }

    pub fn play(&mut self) {
        if self.state().ended {
            self.seek_exact(0.0);
        }
        self.clock.set(None);
        unsafe {
            let _ = self.engine.Play();
        }
    }

    pub fn pause(&mut self) {
        unsafe {
            let _ = self.engine.Pause();
        }
    }

    pub fn seek(&mut self, seconds: f64) {
        let mut target = if seconds.is_finite() {
            seconds.max(0.0)
        } else {
            0.0
        };
        if let Some(duration) = self.duration() {
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
        unsafe {
            let _ = self.engine.SetVolume(volume as f64);
        }
    }

    pub fn state(&self) -> Playback {
        let error = self.error();
        let ready = self.ready_state();
        let metadata = ready >= MF_MEDIA_ENGINE_READY_HAVE_METADATA.0;
        let position = self
            .deferred
            .unwrap_or_else(|| unsafe { self.engine.GetCurrentTime() });
        let position = if position.is_finite() {
            position.max(0.0)
        } else {
            0.0
        };
        let duration = self.duration();
        let (paused, at_end) = unsafe {
            (
                self.engine.IsPaused().as_bool(),
                self.engine.IsEnded().as_bool(),
            )
        };
        let playing = error.is_none() && !paused && !at_end;
        let buffering = error.is_none()
            && (!metadata
                || playing
                    && (ready < MF_MEDIA_ENGINE_READY_HAVE_FUTURE_DATA.0 || self.stalled(playing, position)));
        let has_video = self
            .has_video
            .or_else(|| metadata.then(|| unsafe { self.engine.HasVideo().as_bool() }));
        Playback {
            position: duration.map_or(position, |d| position.min(d)),
            duration,
            playing,
            buffering,
            ended: error.is_none() && (at_end || playback_ended(position, duration, playing)),
            seeking: error.is_none() && self.seeking(has_video),
            error,
        }
    }

    pub fn take_frame(&mut self) -> Option<Frame> {
        self.apply_deferred();
        if self.ready_state() < MF_MEDIA_ENGINE_READY_HAVE_METADATA.0 {
            return None;
        }
        let has_video = match self.has_video {
            Some(v) => v,
            None => {
                let v = unsafe { self.engine.HasVideo().as_bool() };
                self.has_video = Some(v);
                v
            }
        };
        if !has_video {
            return None;
        }
        let frame = self
            .events
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()?;
        // 위치 찾기가 끝난 뒤 받은 프레임이어야 새 위치의 것이다 — 그 전에 받아 둔 옛 프레임은 보여 주되
        // 찾기를 끝내지 않는다. 표시 시각이 목표에 닿은 프레임도 끝으로 친다.
        let fresh = self.pending.as_ref().is_none_or(|p| {
            self.settled(p)
                && (p.generation == 0 || !frame.seeking && frame.marks.finished_after(p.mark))
        });
        let landed = self
            .pending
            .as_ref()
            .is_some_and(|p| (frame.pts as f64 / TICKS - p.target).abs() <= LANDED_SLACK);
        if fresh || landed {
            self.pending = None;
        }
        self.serial += 1;
        Some(Frame {
            width: frame.width,
            height: frame.height,
            rgba: frame.rgba,
            serial: self.serial,
        })
    }

    pub fn drop_watch(&self) -> DropWatch {
        DropWatch(Arc::clone(&self.events))
    }

    fn stalled(&self, playing: bool, position: f64) -> bool {
        match self.clock.get() {
            Some((at, since)) if playing && at == position => since.elapsed() >= STALLED,
            _ => {
                self.clock.set(playing.then(|| (position, Instant::now())));
                false
            }
        }
    }

    fn ready_state(&self) -> i32 {
        unsafe { self.engine.GetReadyState() as i32 }
    }

    fn duration(&self) -> Option<f64> {
        let d = unsafe { self.engine.GetDuration() };
        (d.is_finite() && d > 0.0).then_some(d)
    }

    fn error(&self) -> Option<String> {
        let reported = *self.events.error.lock().unwrap_or_else(|e| e.into_inner());
        let (code, hr) = reported?;
        (code != MF_MEDIA_ENGINE_ERR_NOERROR.0 || hr.is_err()).then(|| error_text(code, hr))
    }

    fn seek_exact(&mut self, seconds: f64) {
        self.clock.set(None);
        self.seek_generation += 1;
        self.pending = Some(PendingSeek::new(self.seek_generation, seconds));
        if self.ready_state() >= MF_MEDIA_ENGINE_READY_HAVE_METADATA.0 {
            self.issue_seek(seconds);
        } else {
            self.deferred = Some(seconds);
        }
    }

    fn issue_seek(&mut self, seconds: f64) {
        if let Some(p) = self.pending.as_mut() {
            p.mark = self.events.seek_seq.load(Ordering::Acquire);
        }
        unsafe {
            let _ = self.engine.SetCurrentTime(seconds);
        }
    }

    fn apply_deferred(&mut self) {
        if self.deferred.is_some()
            && self.ready_state() >= MF_MEDIA_ENGINE_READY_HAVE_METADATA.0
        {
            if let Some(seconds) = self.deferred.take() {
                self.issue_seek(seconds);
            }
        }
    }

    /// 열기는 메타데이터를 읽었을 때, 위치 찾기는 그 뒤 완료 알림이 오고 엔진이 찾기를 마쳤을 때 끝난다.
    fn settled(&self, pending: &PendingSeek) -> bool {
        let ready = self.ready_state() >= MF_MEDIA_ENGINE_READY_HAVE_METADATA.0;
        let done = ready
            && (pending.generation == 0
                || self.deferred.is_none()
                    && !unsafe { self.engine.IsSeeking().as_bool() }
                    && self.events.marks().finished_after(pending.mark));
        if done && pending.settled_at.get().is_none() {
            pending.settled_at.set(Some(Instant::now()));
        }
        done
    }

    /// 비디오면 완료 뒤 첫 프레임을 꺼낼 때(`take_frame`)까지, 소리만이면 완료까지.
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
}

impl Drop for Player {
    fn drop(&mut self) {
        self.events.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(source) = &self.source {
            source.close();
        }
        unsafe {
            let _ = self.engine.Pause();
            // 끄지 않으면 엔진의 작업 스레드가 원본 읽기·소리 장치를 쥔 채 남는다.
            let _ = self.engine.Shutdown();
        }
    }
}

/// 엔진을 깨우고 새 프레임을 읽는 일꾼. 프레임을 받을 텍스처와 장치는 이 스레드만 쓴다.
struct Worker {
    engine: IMFMediaEngine,
    gpu: Gpu,
    events: Arc<Events>,
}

// Media Foundation 재생 엔진은 어느 스레드에서 불러도 되고(자유 스레드), D3D11 장치는 다중 스레드
// 보호를 켜 두었다. 감싼 COM 타입이 그 사실을 몰라 `Send` 가 아닐 뿐이다.
unsafe impl Send for Worker {}

impl Worker {
    fn run(self) {
        let mut target = None;
        while !self.events.stop.load(Ordering::Acquire) {
            let (busy, has_video) = unsafe {
                let ready = self.engine.GetReadyState() as i32
                    >= MF_MEDIA_ENGINE_READY_HAVE_METADATA.0;
                (
                    !ready || !self.engine.IsPaused().as_bool() || self.engine.IsSeeking().as_bool(),
                    !ready || self.engine.HasVideo().as_bool(),
                )
            };
            // 소리만 있는 파일은 깨우지 않아도 시계가 간다(실측) — 일꾼이 할 일이 없다.
            if !has_video {
                return;
            }
            let marks = self.events.marks();
            let seeking = unsafe { self.engine.IsSeeking().as_bool() };
            if let Some(pts) = self.new_frame_time() {
                if let Some((width, height, rgba)) = self.transfer(&mut target) {
                    *self.events.latest.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(Captured {
                            pts,
                            marks,
                            seeking,
                            width,
                            height,
                            rgba,
                        });
                }
            }
            std::thread::sleep(if busy { TICK_BUSY } else { TICK_IDLE });
        }
    }

    /// 표시할 새 프레임이 있으면 그 표시 시각(100ns). 엔진은 새 프레임이 없을 때 `S_FALSE` 로 답하는데
    /// 감싼 메서드는 그것도 성공으로 넘기므로 원래 반환값을 직접 본다.
    fn new_frame_time(&self) -> Option<i64> {
        let mut pts = 0i64;
        let hr = unsafe {
            (Interface::vtable(&self.engine).OnVideoStreamTick)(
                Interface::as_raw(&self.engine),
                &mut pts,
            )
        };
        (hr == S_OK).then_some(pts)
    }

    /// 지금 프레임을 표시 크기의 BGRA 텍스처로 받아 CPU 로 읽는다. 크기 조정·색 변환·표시 회전은 엔진이
    /// GPU 로 한다.
    fn transfer(&self, target: &mut Option<Target>) -> Option<(u32, u32, Vec<u8>)> {
        let (mut cx, mut cy) = (0u32, 0u32);
        unsafe { self.engine.GetNativeVideoSize(Some(&mut cx), Some(&mut cy)) }.ok()?;
        let (width, height) = scaled_size(cx, cy)?;
        if target
            .as_ref()
            .is_none_or(|t| (t.width, t.height) != (width, height))
        {
            *target = Some(self.gpu.target(width, height).ok()?);
        }
        let target = target.as_ref()?;
        unsafe {
            let rect = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            let black = MFARGB {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbAlpha: 255,
            };
            self.engine
                .TransferVideoFrame(&target.texture, None, &rect, Some(&black))
                .ok()?;
            let context = &self.gpu.context;
            context.CopyResource(&target.staging, &target.texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(&target.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .ok()?;
            let stride = mapped.RowPitch as usize;
            let rgba = if mapped.pData.is_null() {
                None
            } else {
                let src = std::slice::from_raw_parts(
                    mapped.pData.cast::<u8>(),
                    stride * height as usize,
                );
                Orientation::IDENTITY.bgra_to_rgba(src, stride, width, height)
            };
            context.Unmap(&target.staging, 0);
            Some((width, height, rgba?))
        }
    }
}

/// `Player` 를 떨어뜨린 뒤 재생 엔진이 실제로 종료됐는지 본다.
pub struct DropWatch(Arc<Events>);

impl DropWatch {
    pub fn released(&self) -> bool {
        self.0.released.load(Ordering::Acquire)
    }
}

/// 엔진이 하드웨어 디코드에 쓰는 장치. 프레임을 받을 텍스처도 같은 장치에 있어야 한다.
struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    manager: IMFDXGIDeviceManager,
}

struct Target {
    width: u32,
    height: u32,
    texture: ID3D11Texture2D,
    staging: ID3D11Texture2D,
}

impl Gpu {
    fn new() -> Result<Self, String> {
        let flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        let mut last = String::from("no device");
        // 그래픽 드라이버가 비디오를 못 받는 기계(원격 데스크톱 등)는 소프트웨어 장치로.
        for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
            let (mut device, mut context) = (None, None);
            let created = unsafe {
                D3D11CreateDevice(
                    None,
                    driver,
                    HMODULE::default(),
                    flags,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            };
            match (created, device, context) {
                (Ok(()), Some(device), Some(context)) => return Self::with(device, context),
                (Err(e), _, _) => last = e.message(),
                _ => {}
            }
        }
        Err(format!("Direct3D 11 is unavailable: {last}"))
    }

    fn with(device: ID3D11Device, context: ID3D11DeviceContext) -> Result<Self, String> {
        unsafe {
            // 디코더는 엔진의 작업 스레드에서, 프레임 읽기는 메인 스레드에서 같은 장치를 쓴다.
            if let Ok(guard) = device.cast::<ID3D11Multithread>() {
                let _ = guard.SetMultithreadProtected(true);
            }
            let (mut token, mut manager) = (0u32, None);
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(failure)?;
            let manager = manager.ok_or("Media Foundation device manager is unavailable")?;
            manager.ResetDevice(&device, token).map_err(failure)?;
            Ok(Self {
                device,
                context,
                manager,
            })
        }
    }

    fn target(&self, width: u32, height: u32) -> windows::core::Result<Target> {
        let mut desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let (mut texture, mut staging) = (None, None);
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut texture))?;
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            self.device
                .CreateTexture2D(&desc, None, Some(&mut staging))?;
        }
        match (texture, staging) {
            (Some(texture), Some(staging)) => Ok(Target {
                width,
                height,
                texture,
                staging,
            }),
            _ => Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_FAIL,
            )),
        }
    }
}

/// 긴 변이 `FRAME_MAX_EDGE` 를 넘을 때만 줄인 크기. 4:2:0 디코더 출력에 맞춰 짝수로 맞춘다.
fn scaled_size(width: u32, height: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }
    let edge = width.max(height);
    if edge <= FRAME_MAX_EDGE {
        return Some((width, height));
    }
    let scale = FRAME_MAX_EDGE as f64 / edge as f64;
    let even = |v: u32| (((v as f64 * scale) / 2.0).round() as u32 * 2).max(2);
    Some((even(width), even(height)))
}

pub fn probe(path: &Path) -> Result<Probe, String> {
    let path = local_file(path)?;
    let (tx, rx) = mpsc::channel();
    // 원본 읽기는 동기 호출이라 막히면 끊을 길이 없다 — 따로 돌리고 기다리는 시간만 정한다.
    std::thread::spawn(move || {
        let _ = tx.send(probe_file(&path));
    });
    rx.recv_timeout(PROBE_TIMEOUT)
        .unwrap_or_else(|_| Err("timed out reading media file".into()))
}

fn probe_file(path: &Path) -> Result<Probe, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        startup()?;
        let reader = source_reader(path)?;
        let mut video = None;
        let mut has_audio = false;
        for index in 0..64 {
            let native = match reader.GetNativeMediaType(index, 0) {
                Ok(t) => t,
                Err(e) if e.code() == MF_E_INVALIDSTREAMNUMBER => break,
                Err(_) => continue,
            };
            match native.GetGUID(&MF_MT_MAJOR_TYPE) {
                Ok(major) if major == MFMediaType_Video && video.is_none() => {
                    video = Some((index, native))
                }
                Ok(major) if major == MFMediaType_Audio => has_audio = true,
                _ => {}
            }
        }
        if video.is_none() && !has_audio {
            return Err("file has no audio or video track".into());
        }
        let duration_s = reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .ok()
            .and_then(|v| u64::try_from(&v).ok())
            .map(|ticks| ticks as f64 / TICKS)
            .filter(|d| *d > 0.0);
        let mut probe = Probe {
            duration_ms: duration_s.map(|d| (d * 1000.0).round() as u64),
            has_audio,
            has_video: video.is_some(),
            ..Probe::default()
        };
        if let Some((index, native)) = video {
            let orientation = rotation(native.GetUINT32(&MF_MT_VIDEO_ROTATION).unwrap_or(0));
            if let Some((_, _, w, h)) = visible_area(&native) {
                let (w, h) = orientation.output_size(w, h);
                probe.width = Some(w);
                probe.height = Some(h);
            }
            probe.poster = poster_png(&reader, index, orientation, duration_s.unwrap_or(0.0));
        }
        Ok(probe)
    }
}

unsafe fn source_reader(path: &Path) -> Result<IMFSourceReader, String> {
    let stream = MFCreateFile(
        MF_ACCESSMODE_READ,
        MF_OPENMODE_FAIL_IF_NOT_EXIST,
        MF_FILEFLAGS_NONE,
        &HSTRING::from(path.as_os_str()),
    )
    .map_err(|e| format!("cannot open media file: {}", e.message()))?;
    let attributes = attributes(1)?;
    // 포스터를 RGB32 로 받으려면 디코더 뒤에 색 변환을 붙여야 한다.
    attributes
        .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
        .map_err(failure)?;
    MFCreateSourceReaderFromByteStream(&stream, &attributes)
        .map_err(|_| "file is not playable audio or video".to_string())
}

unsafe fn poster_png(
    reader: &IMFSourceReader,
    index: u32,
    orientation: Orientation,
    duration: f64,
) -> Option<Vec<u8>> {
    reader
        .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
        .ok()?;
    reader.SetStreamSelection(index, true).ok()?;
    let wanted = MFCreateMediaType().ok()?;
    wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ok()?;
    wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).ok()?;
    reader.SetCurrentMediaType(index, None, &wanted).ok()?;
    let current = reader.GetCurrentMediaType(index).ok()?;
    let (frame_w, frame_h) = unpack_size(current.GetUINT64(&MF_MT_FRAME_SIZE).ok()?)?;
    let stride = current
        .GetUINT32(&MF_MT_DEFAULT_STRIDE)
        .map_or(frame_w as i32 * 4, |s| s as i32);
    let (x, y, w, h) = visible_area(&current)?;
    // 첫 프레임은 검은 화면인 영상이 많아 조금 들어간 곳을 고른다.
    let at = ((duration * 0.1).min(1.0) * TICKS) as i64;
    if at > 0 {
        reader
            .SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(at))
            .ok()?;
    }
    // 위치 찾기는 그 앞 키프레임으로 간다 — 목표에 닿을 때까지 디코드해 나간다.
    let mut chosen = None;
    for _ in 0..1000 {
        let (mut flags, mut time, mut sample) = (0u32, 0i64, None);
        reader
            .ReadSample(index, 0, None, Some(&mut flags), Some(&mut time), Some(&mut sample))
            .ok()?;
        if let Some(sample) = sample {
            chosen = Some(sample);
            if time >= at - (0.02 * TICKS) as i64 {
                break;
            }
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            break;
        }
    }
    let buffer = chosen?.ConvertToContiguousBuffer().ok()?;
    let frame = locked_rows(&buffer, frame_w, frame_h, stride)?;
    let row = frame_w as usize * 4;
    let mut visible = Vec::with_capacity(w as usize * h as usize * 4);
    for line in frame.chunks_exact(row).skip(y as usize).take(h as usize) {
        visible.extend_from_slice(&line[x as usize * 4..(x + w) as usize * 4]);
    }
    let rgba = orientation.bgra_to_rgba(&visible, w as usize * 4, w, h)?;
    let (ow, oh) = orientation.output_size(w, h);
    let mut image = image::RgbaImage::from_raw(ow, oh, rgba)?;
    let edge = ow.max(oh);
    if edge > POSTER_MAX_EDGE {
        let scale = POSTER_MAX_EDGE as f64 / edge as f64;
        let fit = |v: u32| ((v as f64 * scale).round() as u32).clamp(1, POSTER_MAX_EDGE);
        image = image::imageops::resize(
            &image,
            fit(ow),
            fit(oh),
            image::imageops::FilterType::Triangle,
        );
    }
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    (png.len() <= POSTER_MAX_BYTES).then_some(png)
}

/// 잠근 버퍼를 위에서 아래로, 줄 사이 여백 없는 BGRA 로 옮긴다. 음수 줄 간격은 아래에서 위로 쌓인 그림이다.
unsafe fn locked_rows(
    buffer: &IMFMediaBuffer,
    width: u32,
    height: u32,
    stride: i32,
) -> Option<Vec<u8>> {
    let row = width as usize * 4;
    let mut out = vec![0u8; row * height as usize];
    if let Ok(two) = buffer.cast::<IMF2DBuffer>() {
        let (mut first, mut pitch) = (std::ptr::null_mut(), 0i32);
        if two.Lock2D(&mut first, &mut pitch).is_ok() {
            let ok = !first.is_null() && pitch.unsigned_abs() as usize >= row;
            if ok {
                for (y, dst) in out.chunks_exact_mut(row).enumerate() {
                    let src = first.offset(y as isize * pitch as isize);
                    dst.copy_from_slice(std::slice::from_raw_parts(src, row));
                }
            }
            let _ = two.Unlock2D();
            return ok.then_some(out);
        }
    }
    let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
    buffer.Lock(&mut data, None, Some(&mut length)).ok()?;
    let step = stride.unsigned_abs() as usize;
    let fits = !data.is_null()
        && step >= row
        && height > 0
        && step * (height as usize - 1) + row <= length as usize;
    if fits {
        let bytes = std::slice::from_raw_parts(data, length as usize);
        for (y, dst) in out.chunks_exact_mut(row).enumerate() {
            let line = if stride < 0 {
                height as usize - 1 - y
            } else {
                y
            };
            dst.copy_from_slice(&bytes[line * step..line * step + row]);
        }
    }
    let _ = buffer.Unlock();
    fits.then_some(out)
}

/// 디코더 출력 중 실제로 보이는 곳 `(x, y, w, h)`. 1080 줄 영상이 1088 줄로 나오는 식의 여백을 걷는다.
unsafe fn visible_area(media_type: &IMFMediaType) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = unpack_size(media_type.GetUINT64(&MF_MT_FRAME_SIZE).ok()?)?;
    let mut area = MFVideoArea::default();
    let bytes = std::slice::from_raw_parts_mut(
        (&mut area as *mut MFVideoArea).cast::<u8>(),
        std::mem::size_of::<MFVideoArea>(),
    );
    if media_type
        .GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None)
        .is_ok()
    {
        let (x, y) = (area.OffsetX.value.max(0) as u32, area.OffsetY.value.max(0) as u32);
        let (aw, ah) = (area.Area.cx.max(0) as u32, area.Area.cy.max(0) as u32);
        if aw > 0 && ah > 0 && x + aw <= w && y + ah <= h {
            return Some((x, y, aw, ah));
        }
    }
    Some((0, 0, w, h))
}

fn unpack_size(packed: u64) -> Option<(u32, u32)> {
    let (w, h) = ((packed >> 32) as u32, packed as u32);
    (w > 0 && h > 0).then_some((w, h))
}

/// `MF_MT_VIDEO_ROTATION` 은 그림이 반시계로 돌아가 있는 각도다 — 표시할 땐 그만큼 시계 방향으로 돌린다.
/// 재생 엔진은 이것을 스스로 적용하고, 포스터를 뽑는 Source Reader 는 적용하지 않는다.
fn rotation(degrees: u32) -> Orientation {
    match degrees % 360 {
        90 => Orientation::from_affine(0.0, 1.0, -1.0, 0.0),
        180 => Orientation::from_affine(-1.0, 0.0, 0.0, -1.0),
        270 => Orientation::from_affine(0.0, -1.0, 1.0, 0.0),
        _ => Orientation::IDENTITY,
    }
}

fn startup() -> Result<(), String> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| {
            unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }
                .map_err(|e| format!("Media Foundation is unavailable: {}", e.message()))
        })
        .clone()
}

unsafe fn attributes(size: u32) -> Result<IMFAttributes, String> {
    let mut attributes = None;
    MFCreateAttributes(&mut attributes, size).map_err(failure)?;
    attributes.ok_or_else(|| "Media Foundation attributes are unavailable".to_string())
}

fn failure(e: windows::core::Error) -> String {
    let message = e.message();
    if message.trim().is_empty() {
        format!("media player failed ({:#010x})", e.code().0)
    } else {
        message.trim().to_string()
    }
}

fn error_text(code: i32, hr: HRESULT) -> String {
    let base = match MF_MEDIA_ENGINE_ERR(code) {
        MF_MEDIA_ENGINE_ERR_ABORTED => "media loading was aborted",
        MF_MEDIA_ENGINE_ERR_NETWORK => "media could not be read",
        MF_MEDIA_ENGINE_ERR_DECODE => "media could not be decoded",
        MF_MEDIA_ENGINE_ERR_SRC_NOT_SUPPORTED => "file is not playable audio or video",
        MF_MEDIA_ENGINE_ERR_ENCRYPTED => "media is encrypted",
        _ => "media could not be played",
    };
    let detail = if hr.is_err() {
        hr.message()
    } else {
        String::new()
    };
    let detail = detail.trim();
    if detail.is_empty() {
        base.to_string()
    } else {
        format!("{base}: {detail}")
    }
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
