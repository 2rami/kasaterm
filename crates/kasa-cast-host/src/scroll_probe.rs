//! 검증 리그 전용 — 사람 화면을 건드리지 않고 60fps 로 움직이는 화면을 만든다(`KASATERM_TEST_SCREEN_SCROLL=<줄>`).
//! 마지막으로 뜬 장(420v)을 장마다 그만큼 위로 밀고 끝을 처음에 이어 붙인다 — S0 측정기의 `--scroll` 과 같다.

use std::ptr::{self, NonNull};

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_video::{
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPixelFormatType,
    CVPixelBufferGetPlaneCount, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferPool,
    CVPixelBufferUnlockBaseAddress,
};

pub(super) struct Scroller {
    pool: CFRetained<CVPixelBufferPool>,
    step: usize,
    offset: usize,
}

// 풀은 압축 일꾼 스레드 하나만 쓴다.
unsafe impl Send for Scroller {}

fn step() -> Option<usize> {
    if !crate::verification_run() {
        return None;
    }
    std::env::var("KASATERM_TEST_SCREEN_SCROLL").ok()?.trim().parse().ok()
}

impl Scroller {
    pub(super) fn wanted() -> bool {
        step().is_some()
    }

    pub(super) fn from_env(width: usize, height: usize) -> Option<Self> {
        let step = step()?;
        // 색 면(Cb·Cr)은 세로가 절반이라 짝수만큼 밀어야 두 면이 같이 움직인다.
        let step = (step.max(2) + 1) & !1;
        let surface = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let format = CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32);
        let (w, h) = (CFNumber::new_i64(width as i64), CFNumber::new_i64(height as i64));
        let attrs = CFDictionary::<CFString, CFType>::from_slices(
            &[
                unsafe { kCVPixelBufferPixelFormatTypeKey },
                unsafe { kCVPixelBufferIOSurfacePropertiesKey },
                unsafe { kCVPixelBufferWidthKey },
                unsafe { kCVPixelBufferHeightKey },
            ],
            &[&format, &surface, &w, &h],
        );
        let mut out: *mut CVPixelBufferPool = ptr::null_mut();
        let status = unsafe { CVPixelBufferPool::create(None, None, Some(attrs.as_opaque()), NonNull::from(&mut out)) };
        let pool = NonNull::new(out).filter(|_| status == 0)?;
        eprintln!("[screen-stream] 검증 스크롤 {step}줄/장");
        Some(Self { pool: unsafe { CFRetained::from_raw(pool) }, step, offset: 0 })
    }

    pub(super) fn next(&mut self, base: &CVPixelBuffer) -> Option<CFRetained<CVPixelBuffer>> {
        if CVPixelBufferGetPixelFormatType(base) != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange {
            return None;
        }
        let mut out: *mut CVPixelBuffer = ptr::null_mut();
        let status = unsafe { CVPixelBufferPool::create_pixel_buffer(None, &self.pool, NonNull::from(&mut out)) };
        let out: CFRetained<CVPixelBuffer> = unsafe { CFRetained::from_raw(NonNull::new(out).filter(|_| status == 0)?) };
        let luma_rows = CVPixelBufferGetHeightOfPlane(base, 0).max(1);
        self.offset = (self.offset + self.step) % luma_rows;
        unsafe {
            CVPixelBufferLockBaseAddress(base, CVPixelBufferLockFlags::ReadOnly);
            CVPixelBufferLockBaseAddress(&out, CVPixelBufferLockFlags(0));
        }
        for plane in 0..CVPixelBufferGetPlaneCount(base).min(CVPixelBufferGetPlaneCount(&out)) {
            let rows = CVPixelBufferGetHeightOfPlane(base, plane).min(CVPixelBufferGetHeightOfPlane(&out, plane));
            let shift = self.offset * rows / luma_rows;
            let (src, dst) = (
                CVPixelBufferGetBaseAddressOfPlane(base, plane).cast::<u8>(),
                CVPixelBufferGetBaseAddressOfPlane(&out, plane).cast::<u8>(),
            );
            let (ss, ds) = (CVPixelBufferGetBytesPerRowOfPlane(base, plane), CVPixelBufferGetBytesPerRowOfPlane(&out, plane));
            if src.is_null() || dst.is_null() || rows == 0 {
                continue;
            }
            for y in 0..rows {
                let from = (y + shift) % rows;
                unsafe { ptr::copy_nonoverlapping(src.add(from * ss), dst.add(y * ds), ss.min(ds)) };
            }
        }
        unsafe {
            CVPixelBufferUnlockBaseAddress(&out, CVPixelBufferLockFlags(0));
            CVPixelBufferUnlockBaseAddress(base, CVPixelBufferLockFlags::ReadOnly);
        }
        Some(out)
    }
}
