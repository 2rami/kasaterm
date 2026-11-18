//! Windows 소리 — 뜨기는 기본 출력 장치의 WASAPI 루프백(앱 프로세스·사용자 세션), 틀기는 같은 장치의 공유 모드 출력.
//! 둘 다 장치 섞기 형식(보통 f32·48kHz·2채널 이상)을 그대로 열고 화면 스트림 형식과는 여기서 바꾼다(`Mix`·`Linear`) —
//! 형식 바꾸기를 WASAPI(AUTOCONVERTPCM)에 맡기면 루프백에서 되는지가 판마다 달라 시험할 수 있는 한 길로 간다.
//! 화면 도우미(SYSTEM, `win_agent.rs`)는 보안 데스크톱 때문에 있는 것이라 소리는 뜨지 않는다.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use kasa_remote::screen_stream::audio::Jitter;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use super::audio::{FromWire, Mix, Sample, ToWire};

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// 장치 버퍼 20ms(100ns 단위) — 뜨기는 5ms 마다 비우고, 틀기는 장치 주기(보통 10ms)마다 채운다.
const BUFFER_HNS: i64 = 200_000;
const OPEN_WAIT: Duration = Duration::from_secs(5);

/// 기본 출력 장치와 그 섞기 형식. 형식은 다 쓰면 `CoTaskMemFree` 한다.
fn open_default() -> Result<(IAudioClient, *mut WAVEFORMATEX), String> {
    unsafe {
        let devices: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| format!("장치 목록: {e}"))?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|e| format!("기본 출력 장치: {e}"))?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| format!("장치 열기: {e}"))?;
        let format = client.GetMixFormat().map_err(|e| format!("섞기 형식: {e}"))?;
        Ok((client, format))
    }
}

fn mix_of(format: *const WAVEFORMATEX) -> Option<Mix> {
    // 두 구조체는 1바이트 정렬(packed)이라 참조 없이 통째로 복사해 읽는다.
    let f: WAVEFORMATEX = unsafe { format.read_unaligned() };
    let (tag, bits, extra) = (f.wFormatTag, f.wBitsPerSample, f.cbSize);
    let kind = if tag == FORMAT_EXTENSIBLE && extra >= 22 {
        let x: WAVEFORMATEXTENSIBLE = unsafe { format.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
        let sub = x.SubFormat;
        sub.data1 as u16
    } else {
        tag
    };
    let sample = match (kind, bits) {
        (FORMAT_FLOAT, 32) => Sample::F32,
        (FORMAT_PCM, 16) => Sample::I16,
        (FORMAT_PCM, 32) => Sample::I32,
        _ => return None,
    };
    let (rate, channels) = (f.nSamplesPerSec, f.nChannels);
    Some(Mix { rate, channels: usize::from(channels.max(1)), sample })
}

/// 기본 출력 장치를 섞기 형식 그대로 공유 모드로 연다.
fn open_with(flags: u32) -> Result<(IAudioClient, Mix), String> {
    let (client, format) = open_default()?;
    let mix = mix_of(format);
    let opened = match mix {
        Some(_) => unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_HNS, 0, format, None) }
            .map_err(|e| format!("장치 초기화: {e}")),
        None => Err("모르는 섞기 형식".to_string()),
    };
    unsafe { CoTaskMemFree(Some(format as *const _)) };
    opened?;
    Ok((client, mix.ok_or("모르는 섞기 형식")?))
}

/// 열기 결과를 기다린다 — 장치 스레드가 COM 을 스레드마다 쥐어서 열기도 그 스레드에서 한다.
fn wait_open(stop: &AtomicBool, thread: JoinHandle<()>, ready: mpsc::Receiver<Result<(), String>>) -> Result<JoinHandle<()>, String> {
    match ready.recv_timeout(OPEN_WAIT) {
        Ok(Ok(())) => Ok(thread),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            Err("소리 장치가 5초 안에 열리지 않았다".into())
        }
    }
}

struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------- 뜨기

pub(super) struct Source;

impl kasa_remote::screen_stream::audio::Source for Source {
    fn start(&self, out: Box<dyn FnMut(f64, &[i16]) + Send>) -> Result<Box<dyn Send>, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let s = stop.clone();
        let thread = std::thread::Builder::new()
            .name("screen-audio-capture".into())
            .spawn(move || capture_loop(&s, out, tx))
            .map_err(|e| e.to_string())?;
        let thread = wait_open(&stop, thread, rx)?;
        Ok(Box::new(Running { stop, thread: Some(thread) }))
    }
}

/// 5ms 마다 루프백 꾸러미를 비운다. 루프백은 소리가 없으면 꾸러미를 안 준다 — 덩이 시각이 끊기면 받는 쪽 자르개가 새로 센다.
/// 장치가 바뀌거나 사라지면(AUDCLNT_E_DEVICE_INVALIDATED) 기본 장치를 다시 연다.
fn capture_loop(stop: &AtomicBool, mut out: Box<dyn FnMut(f64, &[i16]) + Send>, ready: mpsc::Sender<Result<(), String>>) {
    super::win::com();
    let mut ready = Some(ready);
    let mut frames = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let opened = open_with(AUDCLNT_STREAMFLAGS_LOOPBACK).and_then(|(client, mix)| {
            let capture: IAudioCaptureClient = unsafe { client.GetService() }.map_err(|e| format!("루프백: {e}"))?;
            unsafe { client.Start() }.map_err(|e| format!("루프백 시작: {e}"))?;
            Ok((client, capture, mix))
        });
        let (client, capture, mix) = match opened {
            Ok(o) => {
                if let Some(r) = ready.take() {
                    let _ = r.send(Ok(()));
                }
                o
            }
            Err(e) => {
                if let Some(r) = ready.take() {
                    let _ = r.send(Err(e));
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        let mut wire = ToWire::new(mix.rate);
        'pump: while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
            loop {
                match unsafe { capture.GetNextPacketSize() } {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => break 'pump,
                }
                let (mut data, mut count, mut flags, mut qpc) = (std::ptr::null_mut(), 0u32, 0u32, 0u64);
                if unsafe { capture.GetBuffer(&mut data, &mut count, &mut flags, None, Some(&mut qpc)) }.is_err() {
                    break 'pump;
                }
                if data.is_null() || flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 {
                    frames.clear();
                    frames.resize(count as usize, [0.0; 2]);
                } else {
                    let bytes = unsafe { std::slice::from_raw_parts(data, count as usize * mix.frame_bytes()) };
                    mix.read_stereo(bytes, &mut frames);
                }
                let _ = unsafe { capture.ReleaseBuffer(count) };
                let pcm = wire.convert(&frames);
                if !pcm.is_empty() {
                    // QPC 를 100ns 로 바꾼 값이다 — 화면 장 시각(`win::now`, QPC 초)과 같은 시계.
                    out(qpc as f64 / 10_000_000.0, pcm);
                }
            }
        }
        let _ = unsafe { client.Stop() };
    }
}

// ---------------------------------------------------------------- 틀기

/// 기본 출력 장치로 튼다. 장치 주기마다 깨어 빈 만큼 지터 버퍼에서 꺼내 채운다. 버리면 멈춘다.
pub(super) struct Output {
    _running: Running,
}

impl Output {
    pub(super) fn start(jitter: Arc<Mutex<Jitter>>, silent: bool) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let s = stop.clone();
        let thread = std::thread::Builder::new()
            .name("screen-audio-out".into())
            .spawn(move || render_loop(&s, &jitter, silent, tx))
            .map_err(|e| e.to_string())?;
        let thread = wait_open(&stop, thread, rx)?;
        Ok(Self { _running: Running { stop, thread: Some(thread) } })
    }
}

struct Event(HANDLE);

impl Drop for Event {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn render_loop(stop: &AtomicBool, jitter: &Mutex<Jitter>, silent: bool, ready: mpsc::Sender<Result<(), String>>) {
    super::win::com();
    let opened = open_with(AUDCLNT_STREAMFLAGS_EVENTCALLBACK).and_then(|(client, mix)| unsafe {
        let event = Event(CreateEventW(None, false, false, PCWSTR::null()).map_err(|e| format!("사건: {e}"))?);
        client.SetEventHandle(event.0).map_err(|e| format!("사건 걸기: {e}"))?;
        let render: IAudioRenderClient = client.GetService().map_err(|e| format!("출력: {e}"))?;
        let size = client.GetBufferSize().map_err(|e| format!("버퍼: {e}"))?;
        client.Start().map_err(|e| format!("출력 시작: {e}"))?;
        Ok((client, mix, event, render, size))
    });
    let (client, mix, event, render, size) = match opened {
        Ok(o) => {
            let _ = ready.send(Ok(()));
            o
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut from = FromWire::new(mix.rate);
    while !stop.load(Ordering::Relaxed) {
        unsafe { WaitForSingleObject(event.0, 50) };
        let Ok(padding) = (unsafe { client.GetCurrentPadding() }) else { break };
        let n = size.saturating_sub(padding);
        if n == 0 {
            continue;
        }
        let Ok(buf) = (unsafe { render.GetBuffer(n) }) else { break };
        let frames = from.fill(n as usize, jitter);
        let dst = unsafe { std::slice::from_raw_parts_mut(buf, n as usize * mix.frame_bytes()) };
        if silent {
            dst.fill(0);
        } else {
            mix.write_stereo(frames, dst);
        }
        if unsafe { render.ReleaseBuffer(n, 0) }.is_err() {
            break;
        }
    }
    let _ = unsafe { client.Stop() };
}
