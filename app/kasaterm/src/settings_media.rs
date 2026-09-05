use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use image::{AnimationDecoder, ImageDecoder};

use crate::gpu::GpuRenderer;

const GPU_PREFIX: &str = "settings-media:";
const MAX_CACHE_BYTES: usize = 16 << 20;
const MAX_FILE_BYTES: u64 = 8 << 20;
const MAX_REFERENCE_BYTES: u64 = 32 << 20;
const MAX_DECODE_EDGE: u32 = 2048;
const MAX_DECODE_ALLOC: u64 = 32 << 20;
const FACE_EDGE: u32 = 128;
const MOTION_EDGE: u32 = 128;
const REFERENCE_EDGE: u32 = 320;
const GIF_WORK_EDGE: u32 = 256;
const MAX_GIF_FRAMES: usize = 32;
const MAX_PLAN_FACES: usize = 512;

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MediaSource {
    Theme,
    User,
    Bundled,
    Reference,
}

impl MediaSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Theme => "테마 그림",
            Self::User => "내 그림",
            Self::Bundled => "기본 그림",
            Self::Reference => "참조 그림",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MediaIssue {
    TooLarge,
    Invalid,
    CacheFull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MediaStatus {
    Ready { source: MediaSource, frames: u16 },
    Missing,
    Rejected(MediaIssue),
    NotRequested,
}

impl MediaStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Ready { source, .. } => source.label(),
            Self::Missing => "그림 없음",
            Self::Rejected(MediaIssue::TooLarge) => "그림이 너무 큼",
            Self::Rejected(MediaIssue::Invalid) => "그림을 못 읽음",
            Self::Rejected(MediaIssue::CacheFull) => "미리보기 한도 초과",
            Self::NotRequested => "미리보기 준비 전",
        }
    }

    pub(crate) fn is_ready(self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct FaceKey {
    theme: String,
    slug: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct DetailKey {
    theme: String,
    slug: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum MediaKey {
    Face(FaceKey),
    Reference(DetailKey),
    Motion { detail: DetailKey, motion: String },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct MediaPlan {
    faces: Vec<FaceKey>,
    seen_faces: HashSet<FaceKey>,
    detail: Option<DetailKey>,
}

impl MediaPlan {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn include_theme_cards(&mut self, rows: &[crate::socket::ThemeRow]) -> &mut Self {
        for row in rows {
            for (slug, _) in &row.faces {
                self.include_face(&row.id, slug);
            }
        }
        self
    }

    pub(crate) fn include_student_faces<I, S>(&mut self, theme: &str, slugs: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for slug in slugs {
            self.include_face(theme, slug.as_ref());
        }
        self
    }

    pub(crate) fn include_face(&mut self, theme: &str, slug: &str) -> &mut Self {
        if self.faces.len() >= MAX_PLAN_FACES {
            return self;
        }
        let key = FaceKey {
            theme: normalize_theme(theme),
            slug: slug.to_string(),
        };
        if self.seen_faces.insert(key.clone()) {
            self.faces.push(key);
        }
        self
    }

    pub(crate) fn include_student_detail(&mut self, theme: &str, slug: &str) -> &mut Self {
        self.include_face(theme, slug);
        self.detail = Some(DetailKey {
            theme: normalize_theme(theme),
            slug: slug.to_string(),
        });
        self
    }
}

#[derive(Clone)]
struct MediaFrame {
    rgba: Arc<[u8]>,
    width: u32,
    height: u32,
    delay_ms: u32,
    texture_key: String,
}

#[derive(Clone)]
struct MediaEntry {
    status: MediaStatus,
    frames: Vec<MediaFrame>,
    total_ms: u32,
}

impl MediaEntry {
    fn unavailable(status: MediaStatus) -> Self {
        Self {
            status,
            frames: Vec::new(),
            total_ms: 0,
        }
    }

    fn frame_at(&self, elapsed: Duration) -> usize {
        if self.frames.len() < 2 || self.total_ms == 0 {
            return 0;
        }
        let mut at = (elapsed.as_millis() % u128::from(self.total_ms)) as u32;
        for (index, frame) in self.frames.iter().enumerate() {
            if at < frame.delay_ms {
                return index;
            }
            at = at.saturating_sub(frame.delay_ms);
        }
        0
    }

    fn until_next_frame(&self, elapsed: Duration) -> Option<Duration> {
        if self.frames.len() < 2 || self.total_ms == 0 {
            return None;
        }
        let mut at = (elapsed.as_millis() % u128::from(self.total_ms)) as u32;
        for frame in &self.frames {
            if at < frame.delay_ms {
                return Some(Duration::from_millis(u64::from(frame.delay_ms - at)));
            }
            at = at.saturating_sub(frame.delay_ms);
        }
        Some(Duration::from_millis(1))
    }
}

struct DecodedFrame {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    delay_ms: u32,
}

type LoadResult = Result<Option<(MediaSource, Vec<DecodedFrame>)>, MediaIssue>;

pub(crate) struct SettingsMediaCache {
    generation: u64,
    entries: HashMap<MediaKey, MediaEntry>,
    decoded_bytes: usize,
    gpu_reset_pending: AtomicBool,
}

impl Default for SettingsMediaCache {
    fn default() -> Self {
        Self {
            generation: 0,
            entries: HashMap::new(),
            decoded_bytes: 0,
            gpu_reset_pending: AtomicBool::new(true),
        }
    }
}

impl std::fmt::Debug for SettingsMediaCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsMediaCache")
            .field("generation", &self.generation)
            .field("entries", &self.entries.len())
            .field("decoded_bytes", &self.decoded_bytes)
            .finish()
    }
}

impl SettingsMediaCache {
    pub(crate) fn invalidate(&mut self) {
        self.generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        self.entries.clear();
        self.decoded_bytes = 0;
        self.gpu_reset_pending.store(true, Ordering::Release);
    }

    pub(crate) fn refresh(&mut self, plan: &MediaPlan) {
        self.invalidate();

        if let Some(detail) = plan.detail.as_ref() {
            let face_key = MediaKey::Face(FaceKey {
                theme: detail.theme.clone(),
                slug: detail.slug.clone(),
            });
            self.load_and_insert(face_key, load_face(&detail.theme, &detail.slug));

            let ref_key = MediaKey::Reference(detail.clone());
            self.load_and_insert(ref_key, load_reference(&detail.theme, &detail.slug));

            for motion in ["idle", "walk", "wave", "cheer", "gif"] {
                let key = MediaKey::Motion {
                    detail: detail.clone(),
                    motion: motion.to_string(),
                };
                self.load_and_insert(key, load_motion(&detail.theme, &detail.slug, motion));
            }

            let profile_key = MediaKey::Motion {
                detail: detail.clone(),
                motion: "profile".to_string(),
            };
            let profile = self.entries.get(&MediaKey::Face(FaceKey {
                theme: detail.theme.clone(),
                slug: detail.slug.clone(),
            }));
            if let Some(entry) = profile.cloned() {
                self.entries.insert(profile_key, entry);
            }
        }

        for face in &plan.faces {
            let key = MediaKey::Face(face.clone());
            if !self.entries.contains_key(&key) {
                self.load_and_insert(key, load_face(&face.theme, &face.slug));
            }
        }
    }

    pub(crate) fn begin_paint(&self, gpu: &mut GpuRenderer) {
        if self.gpu_reset_pending.swap(false, Ordering::AcqRel) {
            gpu.drop_images_with_prefix(GPU_PREFIX);
        }
    }

    pub(crate) fn decoded_bytes(&self) -> usize {
        self.decoded_bytes
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn face_status(&self, theme: &str, slug: &str) -> MediaStatus {
        self.status(&MediaKey::Face(FaceKey {
            theme: normalize_theme(theme),
            slug: slug.to_string(),
        }))
    }

    pub(crate) fn reference_status(&self, theme: &str, slug: &str) -> MediaStatus {
        self.status(&MediaKey::Reference(DetailKey {
            theme: normalize_theme(theme),
            slug: slug.to_string(),
        }))
    }

    pub(crate) fn motion_status(&self, theme: &str, slug: &str, motion: &str) -> MediaStatus {
        self.status(&motion_key(theme, slug, motion))
    }

    pub(crate) fn draw_face(
        &self,
        gpu: &mut GpuRenderer,
        theme: &str,
        slug: &str,
        rect: (f32, f32, f32, f32),
    ) -> MediaStatus {
        self.draw(
            gpu,
            &MediaKey::Face(FaceKey {
                theme: normalize_theme(theme),
                slug: slug.to_string(),
            }),
            0,
            rect,
            DrawMode::Above,
        )
    }

    pub(crate) fn draw_reference(
        &self,
        gpu: &mut GpuRenderer,
        theme: &str,
        slug: &str,
        rect: (f32, f32, f32, f32),
    ) -> MediaStatus {
        self.draw(
            gpu,
            &MediaKey::Reference(DetailKey {
                theme: normalize_theme(theme),
                slug: slug.to_string(),
            }),
            0,
            rect,
            DrawMode::Contain,
        )
    }

    pub(crate) fn draw_motion_frame(
        &self,
        gpu: &mut GpuRenderer,
        theme: &str,
        slug: &str,
        motion: &str,
        frame: usize,
        rect: (f32, f32, f32, f32),
    ) -> MediaStatus {
        self.draw(
            gpu,
            &motion_key(theme, slug, motion),
            frame,
            rect,
            DrawMode::Above,
        )
    }

    pub(crate) fn draw_motion_preview(
        &self,
        gpu: &mut GpuRenderer,
        theme: &str,
        slug: &str,
        motion: &str,
        elapsed: Duration,
        rect: (f32, f32, f32, f32),
    ) -> MediaStatus {
        let key = motion_key(theme, slug, motion);
        let frame = self
            .entries
            .get(&key)
            .map_or(0, |entry| entry.frame_at(elapsed));
        self.draw(gpu, &key, frame, rect, DrawMode::Above)
    }

    pub(crate) fn next_motion_frame_in(
        &self,
        theme: &str,
        slug: &str,
        motion: &str,
        elapsed: Duration,
    ) -> Option<Duration> {
        self.entries
            .get(&motion_key(theme, slug, motion))
            .and_then(|entry| entry.until_next_frame(elapsed))
    }

    fn status(&self, key: &MediaKey) -> MediaStatus {
        self.entries
            .get(key)
            .map_or(MediaStatus::NotRequested, |entry| entry.status)
    }

    fn load_and_insert(&mut self, key: MediaKey, loaded: LoadResult) {
        let entry = match loaded {
            Ok(Some((source, decoded))) => {
                let bytes = decoded.iter().map(|frame| frame.rgba.len()).sum::<usize>();
                if bytes > MAX_CACHE_BYTES.saturating_sub(self.decoded_bytes) {
                    MediaEntry::unavailable(MediaStatus::Rejected(MediaIssue::CacheFull))
                } else {
                    self.decoded_bytes += bytes;
                    let total_ms = decoded.iter().map(|frame| frame.delay_ms).sum::<u32>();
                    let frames = decoded
                        .into_iter()
                        .enumerate()
                        .map(|(index, frame)| MediaFrame {
                            rgba: Arc::from(frame.rgba),
                            width: frame.width,
                            height: frame.height,
                            delay_ms: frame.delay_ms,
                            texture_key: texture_key(self.generation, &key, index),
                        })
                        .collect::<Vec<_>>();
                    MediaEntry {
                        status: MediaStatus::Ready {
                            source,
                            frames: frames.len().min(u16::MAX as usize) as u16,
                        },
                        frames,
                        total_ms,
                    }
                }
            }
            Ok(None) => MediaEntry::unavailable(MediaStatus::Missing),
            Err(issue) => MediaEntry::unavailable(MediaStatus::Rejected(issue)),
        };
        self.entries.insert(key, entry);
    }

    fn draw(
        &self,
        gpu: &mut GpuRenderer,
        key: &MediaKey,
        frame: usize,
        rect: (f32, f32, f32, f32),
        mode: DrawMode,
    ) -> MediaStatus {
        self.begin_paint(gpu);
        let Some(entry) = self.entries.get(key) else {
            return MediaStatus::NotRequested;
        };
        let Some(frame) = entry.frames.get(frame) else {
            return entry.status;
        };
        if !gpu.has_image(&frame.texture_key) {
            gpu.upload_image(
                &frame.texture_key,
                frame.rgba.as_ref(),
                frame.width,
                frame.height,
            );
        }
        match mode {
            DrawMode::Above => {
                gpu.queue_image_above(&frame.texture_key, rect.0, rect.1, rect.2, rect.3)
            }
            DrawMode::Contain => gpu.queue_image(
                &frame.texture_key,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                1.0,
                0.0,
                0.0,
            ),
        }
        entry.status
    }
}

#[derive(Clone, Copy)]
enum DrawMode {
    Above,
    Contain,
}

fn motion_key(theme: &str, slug: &str, motion: &str) -> MediaKey {
    MediaKey::Motion {
        detail: DetailKey {
            theme: normalize_theme(theme),
            slug: slug.to_string(),
        },
        motion: motion.to_string(),
    }
}

fn texture_key(generation: u64, key: &MediaKey, frame: usize) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    format!("{GPU_PREFIX}{generation}:{:016x}:{frame}", hasher.finish())
}

fn normalize_theme(theme: &str) -> String {
    if theme.is_empty() || theme == kasa_mcp::character::BASE_THEME_KEY {
        kasa_mcp::character::BASE_THEME_KEY.to_string()
    } else {
        theme.to_string()
    }
}

fn is_base_theme(theme: &str) -> bool {
    theme.is_empty() || theme == kasa_mcp::character::BASE_THEME_KEY
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value != "."
        && value != ".."
        && !value.contains(['/', '\\', '\0'])
}

fn theme_sprite_dir(theme: &str) -> Option<(PathBuf, MediaSource)> {
    if is_base_theme(theme) {
        if let Ok(path) = std::env::var("KASATERM_STUDENTS_DIR") {
            if !path.is_empty() {
                return Some((PathBuf::from(path), MediaSource::User));
            }
        }
        return kasa_socket::home_dir()
            .map(|home| (home.join(".config/kasaterm/students"), MediaSource::User));
    }
    safe_component(theme).then_some(())?;
    kasa_mcp::character::themes_root()
        .map(|root| (root.join(theme).join("sprites"), MediaSource::Theme))
}

fn first_existing(dir: &Path, rels: impl IntoIterator<Item = String>) -> Option<PathBuf> {
    rels.into_iter()
        .map(|rel| dir.join(rel))
        .find(|path| path.is_file())
}

fn load_face(theme: &str, slug: &str) -> LoadResult {
    if !safe_component(slug) || (!is_base_theme(theme) && !safe_component(theme)) {
        return Err(MediaIssue::Invalid);
    }
    if let Some((dir, source)) = theme_sprite_dir(theme) {
        if let Some(path) = first_existing(
            &dir,
            [
                crate::sprites::profile_rel(slug, true),
                crate::sprites::profile_rel(slug, false),
            ],
        ) {
            return decode_static_path(
                &path,
                FACE_EDGE,
                MAX_FILE_BYTES,
                image::imageops::FilterType::Lanczos3,
            )
            .map(|frame| Some((source, vec![frame])));
        }
    }
    let Some(bytes) = crate::sprites::student_profile_png(slug) else {
        return Ok(None);
    };
    decode_static_bytes(bytes, FACE_EDGE, image::imageops::FilterType::Lanczos3)
        .map(|frame| Some((MediaSource::Bundled, vec![frame])))
}

fn load_reference(theme: &str, slug: &str) -> LoadResult {
    if is_base_theme(theme) || !safe_component(theme) || !safe_component(slug) {
        return Ok(None);
    }
    let Some((path, _)) = crate::settings::themegen_ref_info(theme, slug) else {
        return Ok(None);
    };
    decode_static_path(
        &path,
        REFERENCE_EDGE,
        MAX_REFERENCE_BYTES,
        image::imageops::FilterType::Lanczos3,
    )
    .map(|frame| Some((MediaSource::Reference, vec![frame])))
}

fn load_motion(theme: &str, slug: &str, motion: &str) -> LoadResult {
    if !safe_component(slug)
        || (!is_base_theme(theme) && !safe_component(theme))
        || !matches!(motion, "idle" | "walk" | "wave" | "cheer" | "gif")
    {
        return Err(MediaIssue::Invalid);
    }
    if motion == "gif" {
        return load_gif_motion(theme, slug);
    }
    let count = crate::sprites::motion_frame_count(motion);
    if let Some((dir, source)) = theme_sprite_dir(theme) {
        for foldered in [true, false] {
            let paths = (0..count)
                .map(|index| dir.join(crate::sprites::sprite_rel(slug, motion, index, foldered)))
                .collect::<Vec<_>>();
            if paths.iter().all(|path| path.is_file()) {
                let raw = paths
                    .iter()
                    .map(|path| {
                        decode_image_path(
                            path,
                            GIF_WORK_EDGE,
                            MAX_FILE_BYTES,
                            image::imageops::FilterType::Nearest,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let frames = crop_motion_frames(raw, MOTION_EDGE, 100)?;
                return Ok(Some((source, frames)));
            }
        }
    }
    let Some(bytes) = crate::sprites::student_sprite_png(slug, motion) else {
        return Ok(None);
    };
    let raw = bytes
        .iter()
        .map(|bytes| decode_image_bytes(bytes, GIF_WORK_EDGE, image::imageops::FilterType::Nearest))
        .collect::<Result<Vec<_>, _>>()?;
    let frames = crop_motion_frames(raw, MOTION_EDGE, 100)?;
    Ok(Some((MediaSource::Bundled, frames)))
}

fn load_gif_motion(theme: &str, slug: &str) -> LoadResult {
    if let Some((dir, source)) = theme_sprite_dir(theme) {
        let path = dir.join(crate::sprites::gif_rel(slug));
        if path.is_file() {
            let frames = decode_gif_path(&path)?;
            return Ok(Some((source, frames)));
        }
    }
    let Some(bytes) = crate::sprites::student_idle_gif(slug) else {
        return Ok(None);
    };
    let frames = decode_gif_bytes(bytes)?;
    Ok(Some((MediaSource::Bundled, frames)))
}

fn decode_static_path(
    path: &Path,
    edge: u32,
    file_limit: u64,
    filter: image::imageops::FilterType,
) -> Result<DecodedFrame, MediaIssue> {
    let image = decode_image_path(path, edge, file_limit, filter)?;
    Ok(frame_from_image(image, 0))
}

fn decode_static_bytes(
    bytes: &[u8],
    edge: u32,
    filter: image::imageops::FilterType,
) -> Result<DecodedFrame, MediaIssue> {
    let image = decode_image_bytes(bytes, edge, filter)?;
    Ok(frame_from_image(image, 0))
}

fn decode_image_path(
    path: &Path,
    edge: u32,
    file_limit: u64,
    filter: image::imageops::FilterType,
) -> Result<image::RgbaImage, MediaIssue> {
    let metadata = std::fs::metadata(path).map_err(|_| MediaIssue::Invalid)?;
    if metadata.len() == 0 {
        return Err(MediaIssue::Invalid);
    }
    if metadata.len() > file_limit {
        return Err(MediaIssue::TooLarge);
    }
    let file = std::fs::File::open(path).map_err(|_| MediaIssue::Invalid)?;
    decode_reader(std::io::BufReader::new(file), edge, filter)
}

fn decode_image_bytes(
    bytes: &[u8],
    edge: u32,
    filter: image::imageops::FilterType,
) -> Result<image::RgbaImage, MediaIssue> {
    if bytes.is_empty() {
        return Err(MediaIssue::Invalid);
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(MediaIssue::TooLarge);
    }
    decode_reader(Cursor::new(bytes), edge, filter)
}

fn decode_reader<R: std::io::BufRead + std::io::Seek>(
    reader: R,
    edge: u32,
    filter: image::imageops::FilterType,
) -> Result<image::RgbaImage, MediaIssue> {
    let mut reader = image::ImageReader::new(reader)
        .with_guessed_format()
        .map_err(|_| MediaIssue::Invalid)?;
    reader.limits(image_limits());
    let image = reader.decode().map_err(map_image_error)?;
    Ok(resize_rgba(image.to_rgba8(), edge, filter))
}

fn decode_gif_path(path: &Path) -> Result<Vec<DecodedFrame>, MediaIssue> {
    let metadata = std::fs::metadata(path).map_err(|_| MediaIssue::Invalid)?;
    if metadata.len() == 0 {
        return Err(MediaIssue::Invalid);
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(MediaIssue::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(|_| MediaIssue::Invalid)?;
    decode_gif_bytes(&bytes)
}

fn decode_gif_bytes(bytes: &[u8]) -> Result<Vec<DecodedFrame>, MediaIssue> {
    if bytes.is_empty() {
        return Err(MediaIssue::Invalid);
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(MediaIssue::TooLarge);
    }
    let mut decoder =
        image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(map_image_error)?;
    decoder
        .set_limits(image_limits())
        .map_err(map_image_error)?;
    let mut raw = Vec::new();
    let mut delays = Vec::new();
    for frame in decoder.into_frames().take(MAX_GIF_FRAMES + 1) {
        if raw.len() == MAX_GIF_FRAMES {
            return Err(MediaIssue::TooLarge);
        }
        let frame = frame.map_err(map_image_error)?;
        let (num, den) = frame.delay().numer_denom_ms();
        let delay = if den == 0 {
            100
        } else {
            (num / den.max(1)).clamp(20, 10_000)
        };
        raw.push(resize_rgba(
            frame.into_buffer(),
            GIF_WORK_EDGE,
            image::imageops::FilterType::Nearest,
        ));
        delays.push(delay);
    }
    if raw.is_empty() {
        return Err(MediaIssue::Invalid);
    }
    crop_motion_frames_with_delays(raw, MOTION_EDGE, &delays)
}

fn image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_EDGE);
    limits.max_image_height = Some(MAX_DECODE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    limits
}

fn map_image_error(error: image::ImageError) -> MediaIssue {
    match error {
        image::ImageError::Limits(_) => MediaIssue::TooLarge,
        _ => MediaIssue::Invalid,
    }
}

fn resize_rgba(
    image: image::RgbaImage,
    edge: u32,
    filter: image::imageops::FilterType,
) -> image::RgbaImage {
    if image.width() <= edge && image.height() <= edge {
        image
    } else {
        image::imageops::resize(&image, edge, edge, filter)
    }
}

fn crop_motion_frames(
    frames: Vec<image::RgbaImage>,
    edge: u32,
    delay_ms: u32,
) -> Result<Vec<DecodedFrame>, MediaIssue> {
    let delays = vec![delay_ms; frames.len()];
    crop_motion_frames_with_delays(frames, edge, &delays)
}

fn crop_motion_frames_with_delays(
    frames: Vec<image::RgbaImage>,
    edge: u32,
    delays: &[u32],
) -> Result<Vec<DecodedFrame>, MediaIssue> {
    let Some(first) = frames.first() else {
        return Err(MediaIssue::Invalid);
    };
    if frames.len() != delays.len()
        || frames
            .iter()
            .any(|frame| frame.dimensions() != first.dimensions())
    {
        return Err(MediaIssue::Invalid);
    }
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for frame in &frames {
        for (x, y, pixel) in frame.enumerate_pixels() {
            if pixel[3] > 8 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x0 == u32::MAX {
        return Err(MediaIssue::Invalid);
    }
    frames
        .into_iter()
        .zip(delays.iter().copied())
        .map(|(frame, delay)| {
            let cropped =
                image::imageops::crop_imm(&frame, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image();
            let resized = resize_rgba(cropped, edge, image::imageops::FilterType::Nearest);
            Ok(frame_from_image(resized, delay))
        })
        .collect()
}

fn frame_from_image(image: image::RgbaImage, delay_ms: u32) -> DecodedFrame {
    let (width, height) = image.dimensions();
    DecodedFrame {
        rgba: image.into_raw(),
        width,
        height,
        delay_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_deduplicates_faces_and_normalizes_base_theme() {
        let mut plan = MediaPlan::new();
        plan.include_face("", "midori")
            .include_face(kasa_mcp::character::BASE_THEME_KEY, "midori")
            .include_face("theme", "midori");
        assert_eq!(plan.faces.len(), 2);
        assert_eq!(plan.faces[0].theme, kasa_mcp::character::BASE_THEME_KEY);
    }

    #[test]
    fn frame_clock_uses_each_frame_delay() {
        let make = |delay_ms| MediaFrame {
            rgba: Arc::from(vec![0; 4]),
            width: 1,
            height: 1,
            delay_ms,
            texture_key: String::new(),
        };
        let entry = MediaEntry {
            status: MediaStatus::Ready {
                source: MediaSource::Bundled,
                frames: 2,
            },
            frames: vec![make(40), make(80)],
            total_ms: 120,
        };
        assert_eq!(entry.frame_at(Duration::from_millis(39)), 0);
        assert_eq!(entry.frame_at(Duration::from_millis(40)), 1);
        assert_eq!(entry.frame_at(Duration::from_millis(119)), 1);
        assert_eq!(entry.frame_at(Duration::from_millis(120)), 0);
        assert_eq!(
            entry.until_next_frame(Duration::from_millis(50)),
            Some(Duration::from_millis(70))
        );
    }

    #[test]
    fn motion_crop_keeps_union_stable_across_frames() {
        let mut left = image::RgbaImage::new(20, 20);
        let mut right = image::RgbaImage::new(20, 20);
        left.put_pixel(2, 4, image::Rgba([255, 0, 0, 255]));
        right.put_pixel(17, 15, image::Rgba([0, 0, 255, 255]));
        let frames = crop_motion_frames(vec![left, right], 128, 100).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!((frames[0].width, frames[0].height), (16, 12));
        assert_eq!((frames[1].width, frames[1].height), (16, 12));
    }

    #[test]
    fn decoder_rejects_dimensions_before_large_rgba_allocation() {
        let image =
            image::RgbaImage::from_pixel(MAX_DECODE_EDGE + 1, 1, image::Rgba([1, 2, 3, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        assert_eq!(
            decode_image_bytes(
                bytes.get_ref(),
                FACE_EDGE,
                image::imageops::FilterType::Lanczos3,
            )
            .unwrap_err(),
            MediaIssue::TooLarge
        );
    }

    #[test]
    fn bundled_face_and_motion_fit_the_cache_budget() {
        let mut plan = MediaPlan::new();
        plan.include_student_detail("", "midori");
        let mut cache = SettingsMediaCache::default();
        cache.refresh(&plan);
        assert!(cache.face_status("", "midori").is_ready());
        assert!(cache.motion_status("", "midori", "idle").is_ready());
        assert!(cache.motion_status("", "midori", "gif").is_ready());
        assert!(cache.decoded_bytes() <= MAX_CACHE_BYTES);
    }

    #[test]
    fn missing_slug_is_reported_without_retrying_during_paint() {
        let mut plan = MediaPlan::new();
        plan.include_student_detail("", "not-a-real-student");
        let mut cache = SettingsMediaCache::default();
        cache.refresh(&plan);
        assert_eq!(
            cache.face_status("", "not-a-real-student"),
            MediaStatus::Missing
        );
        assert_eq!(
            cache.motion_status("", "not-a-real-student", "walk"),
            MediaStatus::Missing
        );
        assert_eq!(
            cache.reference_status("", "not-a-real-student"),
            MediaStatus::Missing
        );
    }
}
