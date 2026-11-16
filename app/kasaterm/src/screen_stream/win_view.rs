//! Windows 보기(`docs/screen-stream.md` S3) — 받은 장을 Media Foundation 디코더(D3D11 하드웨어 가속)로 풀고, 비디오 프로세서로
//! 화면 크기 BGRA 공유 텍스처에 옮긴다. 칸 렌더러(wgpu·Vulkan)가 그 텍스처를 NT 핸들로 들여와 복사 없이 그린다.
//! 들여오기를 못 하는 기기(Vulkan 확장이 없거나 GPU 가 다름)면 그때부터 CPU 로 읽어 낸다 — 3024×1964 한 장을 읽어 내는 데만
//! 장당 14ms 가 들어(S3 실측) 기본 길로는 쓰지 않는다.
//!
//! 보내는 쪽에 H.264 를 청한다 — Windows 의 HEVC 풀기는 스토어의 「HEVC 비디오 확장」이 있어야 해서 기기마다 다르다.

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use kasa_remote::screen_stream::Frame;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIResource1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE};
use windows::Win32::Media::MediaFoundation::*;

use super::annexb;
use super::win::{com, first_mft, startup};

const PTS_STEP: i64 = 166_667;
/// 칸이 쥐는 장(`view.rs HELD`)보다 넉넉히 — 칸이 아직 그리는 텍스처를 디코더가 덮어쓰지 않게.
const SLOTS: usize = 6;

/// 칸이 공유 텍스처를 못 들여왔다 — 그 뒤 장은 BGRA 바이트도 싣는다.
static CPU_COPY: AtomicBool = AtomicBool::new(false);

pub(crate) fn zero_copy_result(ok: bool) {
    if !ok && !CPU_COPY.swap(true, Ordering::Relaxed) {
        eprintln!("[screen-stream] 칸이 D3D11 텍스처를 못 들여온다 — CPU 로 읽어 올린다");
    }
}

/// 풀린 장 하나 — 공유 텍스처 한 칸, 그리고 CPU 길이면 BGRA 바이트(줄 간격 `stride`).
#[derive(Clone)]
pub(crate) struct Picture {
    slot: Arc<Slot>,
    bytes: Option<Arc<Vec<u8>>>,
    w: u32,
    h: u32,
    stride: u32,
}

impl Picture {
    pub(crate) fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    /// 칸이 들여올 공유 NT 핸들과 그 텍스처의 번호.
    pub(crate) fn surface(&self) -> (*mut std::ffi::c_void, u64) {
        (self.slot.handle.0, self.slot.key)
    }

    pub(crate) fn with_bgra<R>(&self, f: impl FnOnce(&[u8], u32, u32, u32) -> R) -> Option<R> {
        self.bytes.as_ref().map(|b| f(b, self.w, self.h, self.stride))
    }
}

/// 비디오 프로세서가 쓰는 공유 텍스처 한 칸.
struct Slot {
    tex: ID3D11Texture2D,
    output: ID3D11VideoProcessorOutputView,
    handle: HANDLE,
    /// 칸 렌더러의 들여온 텍스처 열쇠 — 핸들 값은 닫힌 뒤 다시 쓰일 수 있어 따로 센다.
    key: u64,
}

// SAFETY: 장치는 멀티스레드 보호를 켰고, GUI 는 핸들 값만 읽는다.
unsafe impl Send for Slot {}
unsafe impl Sync for Slot {}

impl Drop for Slot {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

pub(crate) struct VideoDecoder {
    gpu: Option<Gpu>,
    mft: Option<Mft>,
    /// CPU 길의 BGRA 버퍼들 — GUI 가 다 놓은 것을 다시 쓴다(장마다 24MB 를 새로 잡으면 페이지 폴트로 몇 ms 씩 든다).
    pool: Vec<Arc<Vec<u8>>>,
    pts: i64,
}

impl VideoDecoder {
    /// 받고 싶은 코덱 — HEVC 확장이 없는 Windows 도 풀 수 있게.
    pub(crate) const CODEC: &'static str = "h264";

    pub(crate) fn new() -> Self {
        Self { gpu: None, mft: None, pool: Vec::new(), pts: 0 }
    }

    pub(crate) fn decode(&mut self, frame: &mut Frame) -> Result<Option<Picture>> {
        let fresh = frame.key
            && !frame.param_sets.is_empty()
            && self.mft.as_ref().is_none_or(|m| m.sets != frame.param_sets || m.display != (frame.width, frame.height));
        if fresh {
            self.mft = None;
            if self.gpu.is_none() {
                self.gpu = Some(Gpu::new()?);
            }
            let gpu = self.gpu.as_ref().context("장치")?;
            self.mft = Some(Mft::new(gpu, &frame.param_sets, (frame.width, frame.height))?);
        }
        let Some(mft) = self.mft.as_mut() else { bail!("키프레임 전의 장") };
        let stream = annexb::join(if frame.key { &frame.param_sets } else { &[] }, &frame.data);
        self.pts += PTS_STEP;
        let mut latest = None;
        while !mft.put(&stream, self.pts)? {
            latest = mft.drain()?.or(latest);
        }
        latest = mft.drain()?.or(latest);
        let Some(sample) = latest else { return Ok(None) };
        self.read(&sample).map(Some)
    }

    fn read(&mut self, sample: &IMFSample) -> Result<Picture> {
        let (Some(gpu), Some(mft)) = (self.gpu.as_ref(), self.mft.as_mut()) else { bail!("디코더 없음") };
        let buffer = unsafe { sample.GetBufferByIndex(0)? };
        let dx: IMFDXGIBuffer = buffer.cast().context("디코더가 GPU 장을 내지 않았다")?;
        let mut raw = std::ptr::null_mut();
        unsafe { dx.GetResource(&ID3D11Texture2D::IID, &mut raw)? };
        let tex = unsafe { ID3D11Texture2D::from_raw(raw) };
        let slice = unsafe { dx.GetSubresourceIndex()? };
        if mft.convert.is_none() {
            mft.convert = Some(Convert::new(gpu, mft.coded, mft.display)?);
        }
        let convert = mft.convert.as_mut().context("변환기")?;
        let slot = convert.run(gpu, &tex, slice)?;
        let (w, h) = mft.display;
        if !CPU_COPY.load(Ordering::Relaxed) {
            return Ok(Picture { slot, bytes: None, w, h, stride: w * 4 });
        }
        unsafe { gpu.context.CopyResource(&convert.staging, &slot.tex) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe { gpu.context.Map(&convert.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))? };
        let stride = mapped.RowPitch;
        let len = stride as usize * h as usize;
        let mut bytes = match self.pool.iter().position(|b| Arc::strong_count(b) == 1) {
            Some(i) => self.pool.swap_remove(i),
            None => Arc::new(Vec::new()),
        };
        let copied = Arc::get_mut(&mut bytes).map(|v| {
            v.resize(len, 0);
            unsafe { std::ptr::copy_nonoverlapping(mapped.pData.cast::<u8>(), v.as_mut_ptr(), len) };
        });
        unsafe { gpu.context.Unmap(&convert.staging, 0) };
        copied.context("버퍼를 다른 쪽이 쥐고 있다")?;
        self.pool.push(bytes.clone());
        Ok(Picture { slot, bytes: Some(bytes), w, h, stride })
    }
}

/// 디코더와 변환이 함께 쓰는 장치. 디코더는 제 스레드에서도 이 장치를 쓴다.
struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    manager: IMFDXGIDeviceManager,
}

impl Gpu {
    fn new() -> Result<Self> {
        com();
        startup().map_err(|e| anyhow!("Media Foundation: {e}"))?;
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?
        };
        let device: ID3D11Device = device.context("D3D11 장치")?;
        let context = context.context("D3D11 문맥")?;
        if let Ok(m) = device.cast::<ID3D11Multithread>() {
            let _ = unsafe { m.SetMultithreadProtected(true) };
        }
        let (mut token, mut manager) = (0u32, None);
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager)? };
        let manager = manager.context("MF 장치 관리자")?;
        unsafe { manager.ResetDevice(&device, token)? };
        Ok(Self { device, context, manager })
    }
}

struct Mft {
    mft: IMFTransform,
    sets: Vec<Vec<u8>>,
    /// 디코더가 내는 텍스처 크기(16 배수로 맞춘 부호화 크기).
    coded: (u32, u32),
    /// 보낸 화면 크기.
    display: (u32, u32),
    convert: Option<Convert>,
}

impl Mft {
    fn new(gpu: &Gpu, sets: &[Vec<u8>], display: (u32, u32)) -> Result<Self> {
        let (subtype, name) = if sets.len() >= 3 { (MFVideoFormat_HEVC, "HEVC") } else { (MFVideoFormat_H264, "H.264") };
        let activate = first_mft(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(subtype),
            None,
        )
        .with_context(|| format!("{name} 디코더가 없다"))?;
        let mft: IMFTransform = unsafe { activate.ActivateObject()? };
        unsafe {
            let attrs = mft.GetAttributes()?;
            if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                bail!("{name} 디코더가 D3D11 가속을 못 한다");
            }
            // 재정렬 대기 없이 한 장 넣으면 그 장이 바로 나오게(보내는 쪽은 B 장이 없다).
            attrs.SetUINT32(&MF_LOW_LATENCY, 1)?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager.as_raw() as usize)?;
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(display.0) << 32) | u64::from(display.1))?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mft.SetInputType(0, &input, 0)?;
        }
        let coded = set_output(&mft)?;
        let info = unsafe { mft.GetOutputStreamInfo(0)? };
        if info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32 == 0 {
            bail!("{name} 디코더가 GPU 장을 내지 않는다");
        }
        unsafe {
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(Self { mft, sets: sets.to_vec(), coded, display, convert: None })
    }

    /// 못 받으면(앞 장이 아직 안 나감) 거짓 — 먼저 꺼내고 다시 넣는다.
    fn put(&self, stream: &[u8], pts: i64) -> Result<bool> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(stream.len() as u32)?;
            let mut p = std::ptr::null_mut();
            buffer.Lock(&mut p, None, None)?;
            std::ptr::copy_nonoverlapping(stream.as_ptr(), p, stream.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(stream.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(PTS_STEP)?;
            match self.mft.ProcessInput(0, &sample, 0) {
                Ok(()) => Ok(true),
                Err(e) if e.code() == MF_E_NOTACCEPTING => Ok(false),
                Err(e) => Err(e.into()),
            }
        }
    }

    /// 나올 장을 다 꺼내 마지막 것만 — 앞 장들은 이미 늦었다.
    fn drain(&mut self) -> Result<Option<IMFSample>> {
        let mut last = None;
        loop {
            let mut out = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(None),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let got = unsafe { self.mft.ProcessOutput(0, &mut out, &mut status) };
            let sample = unsafe { ManuallyDrop::take(&mut out[0].pSample) };
            unsafe { ManuallyDrop::drop(&mut out[0].pEvents) };
            match got {
                Ok(()) => last = sample.or(last),
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(last),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.coded = set_output(&self.mft)?;
                    self.convert = None;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// NV12 출력을 고르고 그 부호화 크기를 돌려준다.
fn set_output(mft: &IMFTransform) -> Result<(u32, u32)> {
    for i in 0..64 {
        let t = unsafe { mft.GetOutputAvailableType(0, i) }.context("NV12 출력이 없다")?;
        if unsafe { t.GetGUID(&MF_MT_SUBTYPE)? } == MFVideoFormat_NV12 {
            unsafe { mft.SetOutputType(0, &t, 0)? };
            let size = unsafe { t.GetUINT64(&MF_MT_FRAME_SIZE)? };
            return Ok(((size >> 32) as u32, size as u32));
        }
    }
    bail!("NV12 출력이 없다")
}

/// 디코더의 NV12 텍스처 배열 한 칸 → 화면 크기 BGRA 공유 텍스처(고리).
struct Convert {
    vd: ID3D11VideoDevice,
    vc: ID3D11VideoContext,
    en: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
    slots: Vec<Arc<Slot>>,
    next: usize,
    /// CPU 길에서만 쓰는 읽기 사본.
    staging: ID3D11Texture2D,
    /// 변환이 GPU 에서 끝났는지 — 칸(Vulkan)은 D3D11 의 순서를 모르니 다 쓴 텍스처만 넘긴다.
    done: ID3D11Query,
    /// 디코더 텍스처 배열(주소, 칸)마다 입력 보기 — 디코더는 같은 배열을 돌려 쓴다.
    inputs: Vec<((usize, u32), ID3D11VideoProcessorInputView)>,
}

impl Convert {
    fn new(gpu: &Gpu, coded: (u32, u32), display: (u32, u32)) -> Result<Self> {
        static NEXT_KEY: AtomicU64 = AtomicU64::new(1);
        let vd: ID3D11VideoDevice = gpu.device.cast()?;
        let vc: ID3D11VideoContext = gpu.context.cast()?;
        let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: coded.0,
            InputHeight: coded.1,
            OutputFrameRate: rate,
            OutputWidth: display.0,
            OutputHeight: display.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let en = unsafe { vd.CreateVideoProcessorEnumerator(&desc)? };
        let vp = unsafe { vd.CreateVideoProcessor(&en, 0)? };
        let rect = RECT { left: 0, top: 0, right: display.0 as i32, bottom: display.1 as i32 };
        unsafe {
            // 보내는 쪽 꼴: YCbCr BT.709(비트 2)·16–235(비트 4–5 = 1) → RGB 0–255(0).
            vc.VideoProcessorSetStreamColorSpace(&vp, 0, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 4 | (1 << 4) });
            vc.VideoProcessorSetOutputColorSpace(&vp, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 });
            vc.VideoProcessorSetStreamFrameFormat(&vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            // 부호화 크기는 16 배수로 늘어나 있다(1964 → 1968) — 아래 덧댄 줄을 잘라 낸다.
            vc.VideoProcessorSetStreamSourceRect(&vp, 0, true, Some(&rect));
            vc.VideoProcessorSetStreamDestRect(&vp, 0, true, Some(&rect));
            vc.VideoProcessorSetOutputTargetRect(&vp, true, Some(&rect));
        }
        let mut tex_desc = D3D11_TEXTURE2D_DESC {
            Width: display.0,
            Height: display.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
        };
        let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
        };
        let mut slots = Vec::with_capacity(SLOTS);
        for _ in 0..SLOTS {
            let mut tex = None;
            unsafe { gpu.device.CreateTexture2D(&tex_desc, None, Some(&mut tex))? };
            let tex: ID3D11Texture2D = tex.context("BGRA 텍스처")?;
            let mut output = None;
            unsafe { vd.CreateVideoProcessorOutputView(&tex, &en, &out_desc, Some(&mut output))? };
            let handle = unsafe {
                tex.cast::<IDXGIResource1>()?.CreateSharedHandle(
                    None,
                    DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                    PCWSTR::null(),
                )?
            };
            let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
            slots.push(Arc::new(Slot { tex, output: output.context("출력 보기")?, handle, key }));
        }
        tex_desc.Usage = D3D11_USAGE_STAGING;
        tex_desc.BindFlags = 0;
        tex_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        tex_desc.MiscFlags = 0;
        let mut staging = None;
        unsafe { gpu.device.CreateTexture2D(&tex_desc, None, Some(&mut staging))? };
        let mut done = None;
        unsafe { gpu.device.CreateQuery(&D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 }, Some(&mut done))? };
        Ok(Self {
            vd,
            vc,
            en,
            vp,
            slots,
            next: 0,
            staging: staging.context("읽기 텍스처")?,
            done: done.context("끝 알림")?,
            inputs: Vec::new(),
        })
    }

    /// 다음 칸에 옮기고 GPU 가 다 쓸 때까지 기다린다(실측 1ms 안).
    fn run(&mut self, gpu: &Gpu, tex: &ID3D11Texture2D, slice: u32) -> Result<Arc<Slot>> {
        let key = (tex.as_raw() as usize, slice);
        let input = match self.inputs.iter().find(|(k, _)| *k == key) {
            Some((_, v)) => v.clone(),
            None => {
                let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: slice } },
                };
                let mut view = None;
                unsafe { self.vd.CreateVideoProcessorInputView(tex, &self.en, &desc, Some(&mut view))? };
                let view = view.context("입력 보기")?;
                self.inputs.push((key, view.clone()));
                view
            }
        };
        let slot = self.slots[self.next % self.slots.len()].clone();
        self.next += 1;
        let streams = [D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(input)),
            ..Default::default()
        }];
        let blt = unsafe { self.vc.VideoProcessorBlt(&self.vp, &slot.output, 0, &streams) };
        let [mut stream] = streams;
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        blt?;
        unsafe {
            gpu.context.End(&self.done);
            gpu.context.Flush();
        }
        let mut finished = 0i32;
        let waited = std::time::Instant::now();
        // GetData 는 끝나기 전엔 S_FALSE(성공)를 돌려 `finished` 로 가른다.
        while unsafe { gpu.context.GetData(&self.done, Some((&mut finished as *mut i32).cast()), 4, 0) }.is_err() || finished == 0 {
            if waited.elapsed() > std::time::Duration::from_millis(500) {
                bail!("GPU 변환이 끝나지 않는다");
            }
            std::thread::yield_now();
        }
        Ok(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 받은 장 덤프(`[u32 길이][F 장]`…, `docs/screen-stream.md` S3 「Windows 보기 실측」)를 이 기기 디코더로 풀어 본다.
    /// `KASATERM_TEST_SCREEN_FRAMES=<덤프>` 가 있을 때만. `KASATERM_TEST_SCREEN_OUT=<파일>` 이면 CPU 길로 풀어 마지막 장을
    /// BGRA 그대로 적는다.
    #[test]
    #[ignore = "실기 덤프가 있어야 한다"]
    fn decodes_a_frame_dump() {
        let path = std::env::var("KASATERM_TEST_SCREEN_FRAMES").expect("KASATERM_TEST_SCREEN_FRAMES");
        let frames = parse(&std::fs::read(path).expect("덤프"));
        let size = frames.first().map(|f| (f.width, f.height)).expect("장이 없다");
        let count = frames.len();
        let out = std::env::var("KASATERM_TEST_SCREEN_OUT").ok();
        if out.is_some() {
            zero_copy_result(false);
        }
        let mut dec = VideoDecoder::new();
        let (mut pictures, mut last) = (0, None);
        let mut steady = std::time::Duration::ZERO;
        for (i, mut frame) in frames.into_iter().enumerate() {
            let started = std::time::Instant::now();
            if let Some(p) = dec.decode(&mut frame).expect("풀기") {
                pictures += 1;
                last = Some(p);
            }
            if i > 0 {
                steady += started.elapsed();
            }
        }
        let per = steady.as_secs_f64() * 1000.0 / (count.max(2) - 1) as f64;
        let path = if out.is_some() { "CPU 읽기" } else { "공유 텍스처" };
        eprintln!("[decode-dump] {count} 장 → 풀린 장 {pictures}, 첫 장 뒤 장당 {per:.2}ms({path}), 크기 {size:?}");
        let picture = last.expect("풀린 장");
        assert_eq!(picture.size(), size, "덧댄 줄을 잘라 보낸 크기 그대로");
        assert!(pictures * 10 >= count * 9, "거의 모든 장이 나와야 한다");
        if let Some(out) = out {
            picture.with_bgra(|bgra, w, h, stride| {
                let rows = (0..h as usize).flat_map(|y| &bgra[y * stride as usize..y * stride as usize + w as usize * 4]);
                std::fs::write(out, rows.copied().collect::<Vec<u8>>()).expect("쓰기");
            });
        }
    }

    fn parse(raw: &[u8]) -> Vec<Frame> {
        let mut frames = Vec::new();
        let mut i = 0;
        while let Some(head) = raw.get(i..i + 4) {
            let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
            frames.extend(raw.get(i + 4..i + 4 + len).and_then(Frame::decode));
            i += 4 + len;
        }
        frames
    }
}
