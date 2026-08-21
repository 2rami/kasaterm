//! 학생 스프라이트·프사 **에셋 적재와 드로잉** — render.rs 에서 분리(2026-08-15).
//! 번들/사용자 override 프레임 로딩, idle GIF 캐시, 얼굴·걷기 드로잉 헬퍼.
//! 외부 호출 경로는 render.rs 의 glob 재수출로 종전 `render::…` 그대로다.
use super::*;
use crate::screenread::*;

/// 사용자 override 학생 애셋의 최대 변 길이. 렌더가 슬롯에 contain-fit 하므로
/// 정확한 규격 강제는 불필요 — 사용자가 넣은 초고해상도 원본이 VRAM 을 잡아먹는
/// 것만 방어적으로 막는다(번들 기본 도트는 이미 이 아래라 무영향).
pub(crate) const MAX_STUDENT_EDGE: u32 = 512;

/// 과대 이미지만 contain 다운스케일(종횡비 유지). 그 외엔 원본 그대로.
pub(crate) fn downscale_student(img: image::DynamicImage) -> image::DynamicImage {
    if img.width() > MAX_STUDENT_EDGE || img.height() > MAX_STUDENT_EDGE {
        img.resize(
            MAX_STUDENT_EDGE,
            MAX_STUDENT_EDGE,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        img
    }
}

/// `~/.config/kasaterm/students/<filename>` 을 RGBA 로 읽는다(프사·로고처럼
/// 단일 이미지용). 파일/디렉토리가 없으면 None → 호출측이 번들 기본으로 폴백.
pub(crate) fn user_asset_rgba(filename: &str) -> Option<(Vec<u8>, u32, u32)> {
    user_asset_rgba_in(&crate::socket::students_dir()?, filename)
}

/// dir 주입 버전(테스트용) — students_dir 해석과 분리해 env 없이 검증한다.
pub(crate) fn user_asset_rgba_in(dir: &std::path::Path, filename: &str) -> Option<(Vec<u8>, u32, u32)> {
    let img = downscale_student(image::open(dir.join(filename)).ok()?);
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some((rgba.into_raw(), w, h))
}

/// override 폴더 안 스프라이트 한 장의 상대 경로.
///
/// `foldered` 면 번들과 같은 모션 폴더 구조(`walk/mika-0.png`), 아니면 옛 평면
/// 이름(`mika-walk-0.png`)이다. 옛 이름을 계속 아는 이유는 폴더 분리 이전에
/// 그림을 넣어 둔 사용자가 있어서다 — 구조가 바뀌었다고 그 그림이 하루아침에
/// 무시되면, 사용자 입장에서는 앱이 자기 파일을 잃어버린 것으로 보인다.
pub(crate) fn sprite_rel(slug: &str, motion: &str, i: usize, foldered: bool) -> String {
    if foldered {
        format!("{motion}/{slug}-{i}.png")
    } else if motion == "idle" {
        // 옛 규약은 idle 만 무접미(`slug-N`)였다. walk 외 모션이 이 가지로 새면
        // override 시 wave/cheer 가 idle 프레임으로 둔갑한다.
        format!("{slug}-{i}.png")
    } else {
        format!("{slug}-{motion}-{i}.png")
    }
}

/// override 폴더 안 프사 한 장의 상대 경로 — `sprite_rel` 과 같은 우선순위 규약.
///
/// 웹 라우트(socket.rs `character_face`)도 이걸 쓴다. 두 벌로 두면 네이티브
/// 화면과 웹뷰가 서로 다른 파일을 프사로 고르게 되고, 그건 오류 없이 갈린다.
pub(crate) fn profile_rel(slug: &str, foldered: bool) -> String {
    if foldered {
        format!("profile/{slug}.png")
    } else {
        format!("{slug}-profile.png")
    }
}

/// 모션 프레임 수 — walk 만 6, 나머지는 4.
pub(crate) fn motion_frame_count(motion: &str) -> usize {
    if motion == "walk" { STUDENT_WALK_FRAMES } else { STUDENT_IDLE_FRAMES }
}

/// 한 캐릭터·모션의 사용자 override 스프라이트 프레임 전부를 RgbaImage 로 연다.
/// 프레임이 **하나라도** 없으면 None — 부분 교체(일부만 사용자·일부는 번들)는
/// 애니가 튀므로 all-or-nothing 으로 전체 폴백시킨다.
///
/// 새 폴더 구조와 옛 평면 이름을 **벌 단위로** 가른다. 프레임마다 따로 고르면
/// 새 폴더에 절반만 옮긴 사용자의 애니가 옛 그림과 섞여 튀는데, 그건 위 규칙이
/// 막으려던 바로 그 증상이다.
pub(crate) fn user_sprite_images(slug: &str, motion: &str) -> Option<Vec<image::RgbaImage>> {
    user_sprite_images_in(&crate::socket::students_dir()?, slug, motion)
}

/// dir 주입 버전(테스트용) — students_dir 해석과 분리해 env 없이 검증한다.
pub(crate) fn user_sprite_images_in(
    dir: &std::path::Path,
    slug: &str,
    motion: &str,
) -> Option<Vec<image::RgbaImage>> {
    let n = motion_frame_count(motion);
    let load = |foldered: bool| -> Option<Vec<image::RgbaImage>> {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let p = dir.join(sprite_rel(slug, motion, i, foldered));
            out.push(downscale_student(image::open(p).ok()?).to_rgba8());
        }
        Some(out)
    };
    load(true).or_else(|| load(false))
}

/// 모션 이름 → GPU 텍스처 캐시 키 접두. idle 은 "f"(기존 배너 캐시와 호환),
/// 나머지는 모션 이름 그대로. 프레임 세트는 캐릭터×접두당 1회만 업로드된다.
pub(crate) fn sprite_key_prefix(motion: &str) -> &'static str {
    match motion {
        "idle" => "f",
        "wave" => "wave",
        "cheer" => "cheer",
        "walk" => "walk",
        _ => "f",
    }
}

/// 캐릭터 슬러그 + 모션 → 컴파일타임 내장 도트 프레임(arona-ui 스프라이트의
/// idle/wave/cheer 0..3 · walk-east 0..5). idle=대기, wave=승인 대기(한 팔
/// 인사), cheer=턴 완료(양팔 만세), walk=working(제자리 걸음).
pub(crate) fn student_sprite_png(slug: &str, motion: &str) -> Option<&'static [&'static [u8]]> {
    // idle/wave/cheer 공통 4프레임 — 모션 폴더만 다르고 파일명은 전부 `<slug>-N`.
    macro_rules! frames4 {
        ($n:literal, $d:literal) => {{
            const F: [&[u8]; STUDENT_IDLE_FRAMES] = [
                include_bytes!(concat!("../assets/students/", $d, "/", $n, "-0.png")),
                include_bytes!(concat!("../assets/students/", $d, "/", $n, "-1.png")),
                include_bytes!(concat!("../assets/students/", $d, "/", $n, "-2.png")),
                include_bytes!(concat!("../assets/students/", $d, "/", $n, "-3.png")),
            ];
            &F[..]
        }};
    }
    macro_rules! walk {
        ($n:literal) => {{
            const F: [&[u8]; STUDENT_WALK_FRAMES] = [
                include_bytes!(concat!("../assets/students/walk/", $n, "-0.png")),
                include_bytes!(concat!("../assets/students/walk/", $n, "-1.png")),
                include_bytes!(concat!("../assets/students/walk/", $n, "-2.png")),
                include_bytes!(concat!("../assets/students/walk/", $n, "-3.png")),
                include_bytes!(concat!("../assets/students/walk/", $n, "-4.png")),
                include_bytes!(concat!("../assets/students/walk/", $n, "-5.png")),
            ];
            &F[..]
        }};
    }
    macro_rules! student {
        ($n:literal) => {
            match motion {
                "idle" => frames4!($n, "idle"),
                "wave" => frames4!($n, "wave"),
                "cheer" => frames4!($n, "cheer"),
                "walk" => walk!($n),
                _ => return None,
            }
        };
    }
    // 슬러그 하나가 18개 프레임(`include_bytes!`)으로 펼쳐지므로 arm 을 손으로 쓰면
    // 로스터가 늘 때마다 79줄이 된다. 목록만 두고 arm 은 매크로가 만든다.
    macro_rules! students {
        ($($n:literal),* $(,)?) => {
            match slug {
                $($n => student!($n),)*
                _ => return None,
            }
        };
    }
    // ⚠️`collab-hooks/characters.json` 로스터와 **같은 집합이어야 한다**. 빠진 슬러그는
    // 오류가 아니라 「그림 없는 학생」이 되고, 그러면 배너·스피너 자리가 빈 구멍으로
    // 남는다(`student_has_sprite` 주석 참고). 실제로 에셋을 67명분 만들어 놓고 이
    // 목록에 안 넣어 앱에서는 12명만 보였다(2026-08-11).
    Some(students![
        "arona", "prana", "akane", "akari", "ako", "arisu", "aru", "asuna",
        "atsuko", "ayane", "azusa", "chihiro", "chinatsu", "eimi", "fubuki", "fuuka",
        "hanako", "hare", "haruka", "haruna", "hasumi", "hibiki", "hifumi", "himari",
        "hina", "hinata", "hiyori", "hoshino", "ichika", "iori", "iroha", "izuna",
        "kaho", "kanna", "karin", "kasumi", "kayoko", "kazusa", "kei", "kirino",
        "koharu", "konoka", "kotama", "kotori", "koyuki", "maki", "makoto", "mari",
        "mashiro", "michiru", "midori", "mika", "misaki", "momoi", "mutsuki", "nagisa",
        "neru", "niya", "noa", "nonomi", "rio", "sakurako", "saori", "satsuki",
        "seia", "sena", "serika", "shiroko", "shizuko", "sumire", "toki", "tsubaki",
        "tsukuyo", "tsurugi", "utaha", "wakamo", "yukari", "yuuka", "yuzu",
    ])
}

/// 지금 쓰는 학생 그림을 전부 파일로 꺼낸다(테마 복제용). 쓴 파일 수를 돌려준다.
///
/// 사용자 override 가 있으면 그 파일을, 없으면 번들을 쓴다 — `student_sprite_frames`
/// 와 **같은 우선순위**여야 복제본이 지금 화면과 같은 그림이 된다.
///
/// 번들은 `include_bytes!` 라 경로가 없다. 그래서 디코딩 없이 바이트를 그대로 쓴다
/// (80명 × 18프레임이라 디코딩하면 몇 초가 사람 눈에 그대로 보인다).
pub(crate) fn export_student_sprites(dir: &std::path::Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(dir)?;
    for sub in SPRITE_MOTION_DIRS {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    let src = crate::socket::students_dir();
    // 첫 후보가 쓸 자리고, 뒤는 읽을 자리다 — 로더와 **같은 순서**(새 구조 →
    // 옛 평면 → 번들)여야 복제본이 지금 화면과 같은 그림이 된다.
    let copy = |rels: [String; 2], bundled: Option<&'static [u8]>| -> std::io::Result<bool> {
        let dst = dir.join(&rels[0]);
        let found =
            src.as_ref().and_then(|d| rels.iter().map(|r| d.join(r)).find(|p| p.is_file()));
        if let Some(p) = found {
            // 내보낼 곳이 곧 읽을 곳인 경우가 있다(폴더 열기 seed). 같은 파일을
            // 자기 위에 복사하면 0바이트가 되므로 그때는 이미 제자리다.
            if p != dst {
                std::fs::copy(p, dst)?;
            }
            return Ok(true);
        }
        match bundled {
            Some(b) => {
                std::fs::write(dst, b)?;
                Ok(true)
            }
            None => Ok(false),
        }
    };

    let mut n = 0;
    for (_, slug) in crate::theme::character_slugs() {
        for motion in ["idle", "wave", "cheer", "walk"] {
            let frames = student_sprite_png(slug, motion);
            for i in 0..motion_frame_count(motion) {
                let rels =
                    [sprite_rel(slug, motion, i, true), sprite_rel(slug, motion, i, false)];
                if copy(rels, frames.and_then(|f| f.get(i).copied()))? {
                    n += 1;
                }
            }
        }
        let rels = [profile_rel(slug, true), profile_rel(slug, false)];
        if copy(rels, student_profile_png(slug))? {
            n += 1;
        }
    }
    let logo: &'static [u8] = include_bytes!("../assets/students/schale-logo.png");
    if copy(["schale-logo.png".to_string(), "schale-logo.png".to_string()], Some(logo))? {
        n += 1;
    }
    write_sprite_readme(dir)?;
    Ok(n)
}

/// 그림 폴더의 하위 갈래. 모션 넷과 프사·gif — export 가 미리 만들어 두는 자리라
/// 사용자는 빈 폴더만 보고도 무엇을 어디에 넣는지 안다.
pub(crate) const SPRITE_MOTION_DIRS: [&str; 6] =
    ["idle", "walk", "wave", "cheer", "profile", "gif"];

/// 그림 폴더 사용법을 폴더 안에 남긴다 — **이미 있으면 건드리지 않는다**(사용자가
/// 자기 메모를 적어 뒀을 수 있다).
///
/// 폴더를 열어 준 것만으로는 무엇을 언제 쓰는 그림인지 알 방법이 없다. 규격보다
/// 먼저 "이 모션이 화면 어디서 보이는가"를 적는 이유가 그것이다 — walk 를 고치는
/// 사람이 그게 작업 중 스피너 옆 걸음이라는 걸 알아야 어떤 그림을 그릴지 정한다.
pub(crate) fn write_sprite_readme(dir: &std::path::Path) -> std::io::Result<()> {
    let p = dir.join("README.md");
    if p.exists() {
        return Ok(());
    }
    std::fs::write(p, SPRITE_README)
}

pub(crate) const SPRITE_README: &str = r#"# 학생 그림 폴더

이 폴더에 그림을 넣으면 앱에 들어 있는 기본 도트를 대체한다. 파일이 없는 자리는
기본 도트가 그대로 보이므로, 한 명만 바꾸거나 한 모션만 바꿔도 된다.

## 어떤 모션이 언제 보이나

| 폴더 | 화면에서 보이는 때 | 프레임 |
|---|---|---|
| `idle/` | 세션 시작 배너, 그리고 아무 일도 없을 때 서 있는 모습 | `<이름>-0.png` ~ `-3.png` (4장) |
| `walk/` | claude 가 작업하는 동안 스피너 옆에서 제자리걸음 | `<이름>-0.png` ~ `-5.png` (6장) |
| `wave/` | 승인을 기다릴 때(주황색 선택지가 뜬 상태) 손 흔들기 | `<이름>-0.png` ~ `-3.png` (4장) |
| `cheer/` | 턴이 끝났을 때 양팔 만세 | `<이름>-0.png` ~ `-3.png` (4장) |
| `profile/` | 사이드바·메시지 아바타에 쓰는 얼굴 | `<이름>.png` (1장) |
| `gif/` | 사이드바 카드의 대기 애니메이션(움직이는 얼굴) | `<이름>.gif` (1장) |

`<이름>` 은 로스터의 영문 슬러그다(`mika`, `arona`, `yuuka` …). 설정의 로스터
화면에서 각 학생의 슬러그를 볼 수 있다.

## 규격

- 256×256 PNG, **배경 투명**. 화면이 알아서 자리에 맞춰 줄이므로 정확히 이 크기일
  필요는 없지만, 한 변이 512px 를 넘으면 자동으로 줄인다.
- 한 모션의 프레임은 **크기가 서로 같아야 한다**. 다르면 그 모션은 통째로 기본
  도트로 돌아간다.

## 모션 단위 all-or-nothing

한 모션은 프레임이 **전부 있어야** 쓰인다. `walk/` 에 6장 중 5장만 넣으면 그
모션은 통째로 기본 도트로 돌아간다 — 절반만 바뀐 애니메이션이 튀는 것보다
낫기 때문이다. 다른 모션은 영향받지 않는다.

## 본보기 얻는 법

설정 화면의 그림 폴더 열기 버튼이 **지금 쓰는 그림 전부를 이 구조 그대로** 이
폴더에 풀어 준다. 그 파일을 열어 고치는 것이 규격을 맞추는 가장 빠른 길이다.

## 옛 파일 이름

폴더가 나뉘기 전에는 `mika-0.png` · `mika-walk-0.png` · `mika-profile.png` 처럼
한 폴더에 평평하게 두었다. 그 이름도 계속 읽으므로 기존 파일을 지울 필요는 없다.
다만 같은 그림이 양쪽에 있으면 **폴더 쪽이 이긴다**.
"#;

/// 이 학생 그림이 있나 — **슬롯을 세우기 전에** 물어야 한다.
///
/// ⚠️그리기 직전에 물으면 늦다. 호출부는 스프라이트를 얹을 자리(Clawd 배너·스피너
/// 글리프)를 **먼저 `GridCell::blank()` 으로 지우고** 나서 슬롯을 세운다. 그림이
/// 없으면 `paint_student_overlays` 가 조용히 아무것도 안 올리고 없는 키로 그리기를
/// 부르는데, 그건 에러도 안 나고 아무것도 안 그린다 — 결과는 「폴백」이 아니라
/// **원래 있던 것까지 지워진 빈 구멍**이다. 로스터가 12→79명이 되자 그림 없는
/// 67명에게서 배너와 스피너가 통째로 사라졌다(2026-08-11).
///
/// 싸다: 번들은 `include_bytes` 매치라 디코딩이 없고, override 는 첫 프레임 파일
/// 존재만 본다. 디코딩까지 되는지는 안 본다 — 그건 번들 테스트가 잡는 문제고,
/// 여기서 매 프레임 디코딩할 수는 없다.
pub(crate) fn student_has_sprite(slug: &str, motion: &str) -> bool {
    if student_sprite_png(slug, motion).is_some() {
        return true;
    }
    let Some(dir) = crate::socket::students_dir() else { return false };
    [true, false].into_iter().any(|f| dir.join(sprite_rel(slug, motion, 0, f)).is_file())
}

/// 모션 프레임들을 RGBA로 디코딩하고 투명 여백을 잘라낸다. 크롭은 전 프레임
/// **합집합** 알파 bbox 하나로 — 프레임별 bbox로 자르면 애니의 미세한
/// 키 차이가 contain-fit 배율 차이로 증폭돼 캐릭터가 들썩인다.
/// GPU 텍스처 캐시(`has_image`) 미스 시에만 호출되므로 (캐릭터,모션)당 1회.
pub(crate) fn student_sprite_frames(slug: &str, motion: &str) -> Option<Vec<(Vec<u8>, u32, u32)>> {
    // 사용자 override(students_dir) 전 프레임이 있으면 그걸, 없으면 번들 내장.
    let decoded: Vec<image::RgbaImage> = match user_sprite_images(slug, motion) {
        Some(imgs) => imgs,
        None => {
            let frames = student_sprite_png(slug, motion)?;
            let d: Vec<_> = frames
                .iter()
                .filter_map(|b| image::load_from_memory(b).ok().map(|i| i.to_rgba8()))
                .collect();
            if d.len() != frames.len() {
                return None;
            }
            d
        }
    };
    let (w, h) = decoded[0].dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
    for img in &decoded {
        if img.dimensions() != (w, h) {
            return None;
        }
        for (x, y, p) in img.enumerate_pixels() {
            if p[3] > 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x0 > x1 || y0 > y1 {
        return None; // 전부 투명한 이미지
    }
    Some(
        decoded
            .iter()
            .map(|img| {
                let c = image::imageops::crop_imm(img, x0, y0, x1 - x0 + 1, y1 - y0 + 1)
                    .to_image();
                let (cw, ch) = c.dimensions();
                (c.into_raw(), cw, ch)
            })
            .collect(),
    )
}

/// 캐릭터 슬러그 → statusline 프사 PNG(웹뷰 bust 를 96×96 contain-리사이즈한
/// 정사각 상반신, 컴파일타임 내장).
///
/// ⚠️`student_sprite_png` 의 로스터와 **같은 집합이어야 한다**. 로스터가 12→79명이
/// 될 때 여기만 12명 수동 목록으로 남아, 신규 학생은 SendMessage/tell 프사 자리가
/// 조용히 비었다(2026-08-12 실측: 칸나 발신 메시지에 아바타 없음).
pub(crate) fn student_profile_png(slug: &str) -> Option<&'static [u8]> {
    macro_rules! profiles {
        ($($n:literal),* $(,)?) => {
            match slug {
                $($n => include_bytes!(concat!("../assets/students/profile/", $n, ".png"))
                    as &'static [u8],)*
                _ => return None,
            }
        };
    }
    Some(profiles![
        "arona", "prana", "akane", "akari", "ako", "arisu", "aru", "asuna",
        "atsuko", "ayane", "azusa", "chihiro", "chinatsu", "eimi", "fubuki", "fuuka",
        "hanako", "hare", "haruka", "haruna", "hasumi", "hibiki", "hifumi", "himari",
        "hina", "hinata", "hiyori", "hoshino", "ichika", "iori", "iroha", "izuna",
        "kaho", "kanna", "karin", "kasumi", "kayoko", "kazusa", "kei", "kirino",
        "koharu", "konoka", "kotama", "kotori", "koyuki", "maki", "makoto", "mari",
        "mashiro", "michiru", "midori", "mika", "misaki", "momoi", "mutsuki", "nagisa",
        "neru", "niya", "noa", "nonomi", "rio", "sakurako", "saori", "satsuki",
        "seia", "sena", "serika", "shiroko", "shizuko", "sumire", "toki", "tsubaki",
        "tsukuyo", "tsurugi", "utaha", "wakamo", "yukari", "yuuka", "yuzu",
    ])
}


/// 프사에서 **얼굴 칸용 상단 정사각**이 차지하는 세로 비율.
///
/// 실측으로 고른 값이다(2026-08-21, 학생 6명 × 3비율을 실제 칸 크기로 축소해
/// 대조). `1.0`(자르지 않음)은 얼굴이 뭉개져 누구인지 안 읽히고, `0.5`는 정수리
/// ─ 머리 장식이 캐릭터 식별의 절반이다 ─ 가 잘려 나간다.
const PROFILE_FACE_RATIO: f32 = 0.62;

/// 프사 한 장에서 얼굴 쪽 정사각만 도려내고 원래 변으로 되돌린다.
///
/// 프사가 쓰이는 자리는 전부 작다 — statusbar 12~13px, 세션 열 22px, tell·
/// SendMessage 는 논리 14×17.5px 다. 96² 전신 bust 를 그런 칸에 통째로 넣으면
/// 얼굴이 3~8px 이 되어 사람인지도 안 보인다. **거노 2026-08-21 「학생 프사 tell
/// 에 안 나와」의 실체가 이것이다 — 안 그려진 게 아니라 안 읽히는 것이다.**
///
/// GIF 경로는 같은 문제를 이미 같은 방식으로 풀고 있다(`student_idle_anim` 이
/// 알파 bbox 로 어깨 위 정사각을 도려낸다). 정적 프사에 그 방식을 그대로 쓸 수는
/// 없다 — 이쪽은 96² 에 여백 없이 꽉 찬 에셋이라 알파 bbox 가 그림 전체이고,
/// 그래서 아무것도 안 잘린다(실측). 세로 비율로 자르는 이유가 그것이다.
///
/// 자른 뒤 **원래 변으로 되돌리는** 것은 큰 칸(설정 화면 학생 고르기) 때문이다.
/// 잘린 크기 그대로 두면 그런 자리에서 업스케일이 되어 흐려진다.
fn crop_profile_face(rgba: Vec<u8>, w: u32, h: u32) -> Option<(Vec<u8>, u32, u32)> {
    let edge = w.min(h);
    let side = (edge as f32 * PROFILE_FACE_RATIO).round() as u32;
    // 이미 얼굴만 담긴 작은 에셋이거나 비율이 무의미하면 그대로 — 두 번 자르면
    // 눈만 남는다.
    if side == 0 || side >= edge {
        return Some((rgba, w, h));
    }
    let img = image::RgbaImage::from_raw(w, h, rgba)?;
    // 가로는 가운데, 세로는 위 — 캐릭터는 캔버스 가운데 서 있고 얼굴은 위에 있다.
    let face = image::imageops::crop_imm(&img, (w - side) / 2, 0, side, side).to_image();
    let out =
        image::imageops::resize(&face, edge, edge, image::imageops::FilterType::Lanczos3);
    Some((out.into_raw(), edge, edge))
}

pub(crate) fn student_profile_rgba(slug: &str) -> Option<(Vec<u8>, u32, u32)> {
    let (rgba, w, h) = student_profile_rgba_full(slug)?;
    crop_profile_face(rgba, w, h)
}

/// 자르기 전 프사 원본 — override 우선, 없으면 번들.
fn student_profile_rgba_full(slug: &str) -> Option<(Vec<u8>, u32, u32)> {
    if let Some(dir) = crate::socket::students_dir() {
        for foldered in [true, false] {
            if let Some(r) = user_asset_rgba_in(&dir, &profile_rel(slug, foldered)) {
                return Some(r);
            }
        }
    }
    let img = image::load_from_memory(student_profile_png(slug)?).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

/// 테마 목록의 미리보기 얼굴 — **지정한 그림 하나**를 그린다.
///
/// `draw_student_face` 와 갈라 두는 이유가 있다. 그건 "지금 고른 테마의 그림"을
/// 찾으므로, 테마 목록처럼 **여러 테마를 나란히** 놓는 자리에서는 전부 같은
/// 얼굴이 된다 — 고르기 전에 무엇을 고르는지 보여 주는 게 목적인 화면에서 그건
/// 아무것도 안 보여 주는 것과 같다. `src` 가 파일이면 그 png 를, `None` 이면
/// 바이너리에 박힌 번들 그림을 쓴다(번들 테마 카드).
///
/// 키가 서로 다르므로 `student:` 캐시와 섞이지 않는다 — 테마를 바꿔도
/// `drop_images_with_prefix("student:")` 가 이 미리보기를 건드리지 않고, 대신
/// 테마 목록이 바뀔 때 `theme:` 접두사로 따로 비운다.
pub(crate) fn draw_theme_face(
    g: &mut gpu::GpuRenderer,
    theme_id: &str,
    slug: &str,
    src: Option<&std::path::Path>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> bool {
    let key = format!("theme:{theme_id}:{slug}");
    if !g.has_image(&key) {
        let decoded = match src {
            Some(p) => std::fs::read(p)
                .ok()
                .and_then(|b| image::load_from_memory(&b).ok())
                .map(|i| i.to_rgba8()),
            None => student_profile_png(slug)
                .and_then(|b| image::load_from_memory(b).ok())
                .map(|i| i.to_rgba8()),
        };
        let Some(img) = decoded else { return false };
        let (iw, ih) = img.dimensions();
        g.upload_image(&key, &img.into_raw(), iw, ih);
    }
    if !g.has_image(&key) {
        return false;
    }
    g.queue_image_above(&key, x, y, w, h);
    true
}

/// 테마 전환이 걷히는 데 걸리는 시간. 눈이 "무엇이 바뀌었나"를 읽을 만큼은 길고,
/// 다음 클릭을 기다리게 할 만큼 길지는 않은 자리.
pub(crate) const THEME_FX_SECS: f32 = 0.34;

/// 픽셀 블록 한 변(논리 px). 셀보다 크게 잡아야 "큰 픽셀"로 읽힌다 — 셀 크기에
/// 맞추면 그냥 부드러운 페이드처럼 보이고, 블록 수도 네 배가 된다.
pub(crate) const FX_BLOCK: f32 = 26.0;

/// 테마 전환 디졸브 — 옛 배경색 블록이 가운데서 바깥으로 걷힌다.
///
/// 블록마다 사라질 시점을 「중심에서의 거리」와 「좌표 해시」로 섞어 정한다. 거리만
/// 쓰면 매끈한 원이 퍼져 픽셀이라는 느낌이 안 나고, 해시만 쓰면 방향 없이 지글거리는
/// TV 노이즈가 된다. 둘을 섞어야 퍼지는 물결의 **가장자리가 픽셀로 부서진다**.
pub(crate) fn paint_theme_dissolve(g: &mut gpu::GpuRenderer, t: f32, old_bg: [u8; 4], w: f32, h: f32) {
    let (cx, cy) = (w * 0.5, h * 0.5);
    let max_d = (cx * cx + cy * cy).sqrt().max(1.0);
    let cols = (w / FX_BLOCK).ceil() as i32;
    let rows = (h / FX_BLOCK).ceil() as i32;
    for j in 0..rows {
        for i in 0..cols {
            let (bx, by) = (i as f32 * FX_BLOCK, j as f32 * FX_BLOCK);
            let d = ((bx + FX_BLOCK * 0.5 - cx).powi(2) + (by + FX_BLOCK * 0.5 - cy).powi(2))
                .sqrt()
                / max_d;
            // 좌표를 섞어 0..1 로 흩는 값. 난수를 안 쓰는 건 프레임마다 같은 답이
            // 나와야 블록이 한 번 사라진 뒤 다시 나타나지 않기 때문이다.
            let hash = {
                let n = (i.wrapping_mul(73_856_093) ^ j.wrapping_mul(19_349_663)) as u32;
                (n >> 8 & 0xFFFF) as f32 / 65535.0
            };
            // 거리 70% + 흩뿌림 30%. 앞의 +0.2 는 **첫 프레임에 이미 중앙이 뚫려
            // 있게** 한다 — 0 에서 시작하면 한 프레임 동안 화면 전체가 옛 배경
            // 단색이라 "퍼진다"가 아니라 "깜빡 꺼졌다 켜진다"로 읽힌다.
            if t * 1.1 + 0.2 > d * 0.7 + hash * 0.3 {
                continue;
            }
            round_rect(g, bx, by, FX_BLOCK, FX_BLOCK, 0.0, old_bg);
        }
    }
}

/// 캐릭터 이름 자리에 그 학생의 얼굴을 그린다 — 없는 캐릭터면 아무것도 안 그리고
/// `false` 를 돌려 부르는 쪽이 색 점으로 되돌아가게 한다.
///
/// 업로드는 캐릭터당 한 번(`has_image` 미스일 때만)이라 프레임마다 불러도 싸다.
/// statusline·Info·tell 렌더가 같은 키를 공유하니 어디서 처음 그리든 나머지는
/// 캐시를 탄다.
pub(crate) fn draw_student_face(
    g: &mut gpu::GpuRenderer,
    name: &str,
    x: f32,
    y: f32,
    size: f32,
) -> bool {
    let Some(slug) = theme::character_slug(name) else {
        return false;
    };
    let key = format!("student:{slug}:profile");
    if !g.has_image(&key) {
        let Some((rgba, w, h)) = student_profile_rgba(slug) else {
            return false;
        };
        g.upload_image(&key, &rgba, w, h);
    }
    g.queue_image_above(&key, x, y, size, size);
    true
}

/// 0..1 을 오가는 부드러운 호흡 — `period` 초에 한 번 왕복한다.
///
/// 켰다 끄는 깜빡임과 갈리는 건 **가장자리 시야**에서다. 밝기가 뚝 끊기면 눈이
/// 그쪽으로 끌려가 하던 일을 놓치는데, 이어지면 있다는 것만 알고 지나칠 수 있다.
pub(crate) fn breathe(t: f32, period: f32) -> f32 {
    0.5 - 0.5 * (std::f32::consts::TAU * t / period.max(0.001)).cos()
}

/// 켜짐/꺼짐이 또렷한 깜빡임. `breathe` 와 **역할이 갈린다** — 숨쉬기는 넓은 판을
/// 은근히 밝혀 "있다"를 알리는 것이고, 이건 작은 동그라미 하나를 껐다 켜서 "지금
/// 보라"를 말한다(2026-08-11 지시: "숨쉬기말고 동그라미깜빡이게").
///
/// 숨쉬기를 그 동그라미에 그대로 쓰면 안 되는 이유가 있다. 밝기가 이어지는 신호는
/// 칠하는 넓이가 클 때만 눈에 걸리는데, 6px 점에서는 진폭이 그대로여도 보이는 총량이
/// 1/100 이라 그냥 흐린 점으로 읽힌다 — 카드 전체를 숨쉬게 만들었던 게 애초에 그
/// 때문이었다.
///
/// 완전한 사각파는 아니다. 가장자리 8% 를 램프로 밀어 두는 건 즉각 점멸이 시야
/// 가장자리에서 지나치게 튀어서고(그게 예전 커서 블링크 동기 방식을 걷어낸 이유),
/// 그래도 중간의 켜짐·꺼짐 구간이 평평해 "깜빡인다"로 읽힌다.
/// 깜빡이는 상태 동그라미 — 사이드바에서 "내 손이 필요하다"를 말하는 유일한 모양.
///
/// 꺼짐 구간에도 알파를 0 까지 내리지 않는다. 점이 통째로 사라졌다 나타나면 그
/// 자리에 무언가 있다는 것 자체를 매번 다시 찾게 되고, 여러 방이 동시에 깜빡일 때
/// 목록이 들썩여 보인다. 자국을 남기면 "있다"는 계속 읽히고 깜빡임은 "봐라"만 한다.
pub(crate) fn blink_dot(g: &mut gpu::GpuRenderer, x: f32, y: f32, size: f32, col: [u8; 4], period: f32) {
    let mut c = col;
    c[3] = (70.0 + 185.0 * blink(anim_phase_secs(), period)) as u8;
    circle_rect(g, x, y, size, c);
}

/// 지금 이 순간 깜빡임이 어디쯤인가(0=바닥, 1=꼭대기). 헤드리스 캡처가 밝은 쪽을
/// 골라 찍는 데 쓴다 — 위상에 따라 그림이 달라지면 "글로우가 안 나온다"가 된다.
pub(crate) fn blink_phase(period: f32) -> f32 {
    blink(anim_phase_secs(), period)
}

pub(crate) fn blink(t: f32, period: f32) -> f32 {
    const EDGE: f32 = 0.08;
    let x = (t / period.max(0.001)).rem_euclid(1.0);
    if x < EDGE {
        x / EDGE
    } else if x < 0.5 {
        1.0
    } else if x < 0.5 + EDGE {
        1.0 - (x - 0.5) / EDGE
    } else {
        0.0
    }
}

/// 프로세스 시작 기준 단조증가 초 — 시간으로 도는 그림(로딩바 스윕, idle gif)이
/// 전부 같은 시계를 본다. 펌프가 도는 동안 매 프레임 갱신된다.
pub(crate) fn anim_phase_secs() -> f32 {
    static EPOCH: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
    EPOCH.elapsed().as_secs_f32()
}

/// override 폴더 안 대기 gif 의 상대 경로. 폴더가 나뉘기 전에는 이 자리에 아무것도
/// 안 읽었으므로 옛 평면 이름은 없다 — `sprite_rel` 과 달리 한 가지뿐이다.
pub(crate) fn gif_rel(slug: &str) -> String {
    format!("gif/{slug}.gif")
}

/// 지금 쓰는 대기 gif 의 바이트 — 사용자 폴더가 번들을 덮는다.
pub(crate) fn student_idle_gif_bytes(slug: &str) -> Option<Vec<u8>> {
    if let Some(dir) = crate::socket::students_dir() {
        if let Ok(b) = std::fs::read(dir.join(gif_rel(slug))) {
            return Some(b);
        }
    }
    student_idle_gif(slug).map(|b| b.to_vec())
}

pub(crate) fn student_idle_gif(slug: &str) -> Option<&'static [u8]> {
    Some(match slug {
        "arona" => include_bytes!("../assets/students/gif/arona.gif"),
        "prana" => include_bytes!("../assets/students/gif/prana.gif"),
        "midori" => include_bytes!("../assets/students/gif/midori.gif"),
        "momoi" => include_bytes!("../assets/students/gif/momoi.gif"),
        "yuzu" => include_bytes!("../assets/students/gif/yuzu.gif"),
        "arisu" => include_bytes!("../assets/students/gif/arisu.gif"),
        "yuuka" => include_bytes!("../assets/students/gif/yuuka.gif"),
        "shiroko" => include_bytes!("../assets/students/gif/shiroko.gif"),
        "hoshino" => include_bytes!("../assets/students/gif/hoshino.gif"),
        "koharu" => include_bytes!("../assets/students/gif/koharu.gif"),
        "himari" => include_bytes!("../assets/students/gif/himari.gif"),
        "aru" => include_bytes!("../assets/students/gif/aru.gif"),
        _ => return None,
    })
}

/// 투명하지 않은 픽셀이 차지하는 사각 `(x, y, w, h)`. 빈 그림이면 전체.
pub(crate) fn alpha_bbox(img: &image::RgbaImage) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for (x, y, p) in img.enumerate_pixels() {
        if p.0[3] > 8 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 == u32::MAX {
        return (0, 0, img.width(), img.height());
    }
    (x0, y0, x1 - x0 + 1, y1 - y0 + 1)
}

/// 한 캐릭터의 idle 애니메이션 — 프레임 RGBA 와 각 프레임이 머무는 ms.
pub(crate) struct IdleAnim {
    frames: Vec<(Vec<u8>, u32, u32)>,
    delays_ms: Vec<u32>,
    total_ms: u32,
}

type IdleAnimCache =
    std::sync::Mutex<std::collections::HashMap<String, Option<std::sync::Arc<IdleAnim>>>>;
pub(crate) static IDLE_ANIM_CACHE: std::sync::OnceLock<IdleAnimCache> = std::sync::OnceLock::new();

/// 디코딩된 대기 gif 캐시를 비운다. GPU 텍스처 캐시를 비우는 것만으로는 안 걷힌다
/// — 프레임이 여기 남아 있으면 새로 넣은 gif 파일이 화면까지 오지 못한다.
pub(crate) fn invalidate_idle_anim() {
    if let Some(c) = IDLE_ANIM_CACHE.get() {
        if let Ok(mut c) = c.lock() {
            c.clear();
        }
    }
}

/// idle.gif → 프레임 배열. 캐릭터당 **한 번만** 디코딩해 캐시한다.
///
/// 캔버스는 256² 인데 캐릭터는 그 안에 94×208 짜리 전신 도트로 서 있다. 그걸
/// 통째로 16px 칸에 넣으면 캐릭터 폭이 6px 이 되어 누구인지 못 알아본다 — 그래서
/// 알파 bbox 를 잡아 **어깨 위 정사각**만 도려낸다. 정적 프사가 이미 얼굴에 맞춰
/// 잘린 에셋인 것과 같은 이유고, 덕분에 두 경로가 같은 크기로 읽힌다.
///
/// 픽셀 아트라 축소는 Nearest 로 — 보간을 쓰면 도트가 뭉개져 흐려진다.
pub(crate) fn student_idle_anim(slug: &str) -> Option<std::sync::Arc<IdleAnim>> {
    use std::sync::Arc;
    let cache = IDLE_ANIM_CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(slug).cloned()) {
        return hit;
    }
    let built = (|| {
        use image::AnimationDecoder;
        let bytes = student_idle_gif_bytes(slug)?;
        let dec = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).ok()?;
        let mut frames = Vec::new();
        let mut delays_ms = Vec::new();
        let mut crop: Option<(u32, u32, u32)> = None;
        for f in dec.into_frames().collect_frames().ok()? {
            let (num, den) = f.delay().numer_denom_ms();
            // 0ms 프레임은 브라우저 관행대로 100ms 로 — 그대로 두면 루프 길이가
            // 0 이 되어 나눗셈이 터진다.
            let ms = if den == 0 { 100 } else { (num / den.max(1)).max(20) };
            let mut buf = f.into_buffer();
            // 자를 자리는 **첫 프레임에서 한 번만** 잡는다. 프레임마다 다시 재면
            // 팔이 오르내릴 때 사각이 따라 흔들려 얼굴이 칸 안에서 덜컹거린다.
            let (cx, cy, cs) = *crop.get_or_insert_with(|| {
                let (bx, by, bw, _bh) = alpha_bbox(&buf);
                (bx, by, bw.max(1))
            });
            let cs = cs.min(buf.width().saturating_sub(cx)).min(buf.height().saturating_sub(cy));
            if cs == 0 {
                return None;
            }
            let face = image::imageops::crop(&mut buf, cx, cy, cs, cs).to_image();
            // 쓰는 자리가 16~22px 이라 32² 면 충분하고, 프레임 수만큼 곱해도 가볍다.
            const OUT: u32 = 32;
            let small =
                image::imageops::resize(&face, OUT, OUT, image::imageops::FilterType::Nearest);
            frames.push((small.into_raw(), OUT, OUT));
            delays_ms.push(ms);
        }
        (!frames.is_empty()).then(|| {
            let total_ms = delays_ms.iter().sum::<u32>().max(1);
            Arc::new(IdleAnim { frames, delays_ms, total_ms })
        })
    })();
    if let Ok(mut c) = cache.lock() {
        c.insert(slug.to_string(), built.clone());
    }
    built
}

/// 캐릭터 자리에 **움직이는** 얼굴을 그린다 — `phase` 는 앱이 켜진 뒤 흐른 초.
///
/// gif 가 없는 캐릭터는 정지 프사로 되돌아간다(`draw_student_face`). 프레임마다
/// 텍스처 키가 갈리므로 업로드는 프레임당 한 번뿐이고, 이후로는 큐잉만 한다.
pub(crate) fn draw_student_face_anim(
    g: &mut gpu::GpuRenderer,
    name: &str,
    x: f32,
    y: f32,
    size: f32,
    phase: f32,
) -> bool {
    let Some(slug) = theme::character_slug(name) else {
        return false;
    };
    let Some(anim) = student_idle_anim(slug) else {
        return draw_student_face(g, name, x, y, size);
    };
    let mut at = ((phase * 1000.0) as u32) % anim.total_ms;
    let mut idx = 0;
    for (i, d) in anim.delays_ms.iter().enumerate() {
        if at < *d {
            idx = i;
            break;
        }
        at -= d;
    }
    let key = format!("student:{slug}:idle:{idx}");
    if !g.has_image(&key) {
        let (rgba, w, h) = &anim.frames[idx];
        g.upload_image(&key, rgba, *w, *h);
    }
    g.queue_image_above(&key, x, y, size, size);
    true
}

/// 사이드바 줄에서 **걷는** 학생. 도는 pane 임을 그림 자체로 말한다.
///
/// `draw_student_face_anim` 과 그리는 것이 다르다 — 저쪽은 GIF 를 얼굴만 잘라
/// 쓰는 정지에 가까운 배너고, 이건 pane 안 스피너 자리에 쓰는 것과 같은 도트
/// 스프라이트다(같은 프레임·같은 캐시 키라 텍스처를 두 번 올리지 않는다).
///
/// 스프라이트가 없는 캐릭터·슬러그가 안 잡히는 이름이면 `false` 를 돌려준다 —
/// 호출자가 얼굴로 되돌아갈 수 있어야, 걷기 에셋이 빠진 학생의 줄이 빈칸이 되지
/// 않는다.
pub(crate) fn draw_student_walk(
    g: &mut gpu::GpuRenderer,
    name: &str,
    x: f32,
    y: f32,
    size: f32,
    phase: f32,
) -> bool {
    let Some(slug) = theme::character_slug(name) else {
        return false;
    };
    let idx = ((phase * 1000.0 / STUDENT_WALK_FRAME_MS) as usize) % STUDENT_WALK_FRAMES;
    let key = format!("student:{slug}:walk{idx}");
    if !g.has_image(&key) {
        let Some(frames) = student_sprite_frames(slug, "walk") else {
            return false;
        };
        for (i, (rgba, w, h)) in frames.iter().enumerate() {
            g.upload_image(&format!("student:{slug}:walk{i}"), rgba, *w, *h);
        }
    }
    g.queue_image_above(&key, x, y, size, size);
    true
}

/// SCHALE 로고 PNG → RGBA. agents 뷰 캐시 미스 시 1회 디코딩. 사용자
/// override(students_dir/schale-logo.png) 우선, 없으면 include_bytes 번들.
pub(crate) fn schale_logo_rgba() -> Option<(Vec<u8>, u32, u32)> {
    if let Some(r) = user_asset_rgba("schale-logo.png") {
        return Some(r);
    }
    let img = image::load_from_memory(include_bytes!("../assets/students/schale-logo.png"))
        .ok()?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

/// agents/resume 피커 배경(교실). 셀 뒤에 깔리므로 텍스트 대비 확보를 위해 로드
/// 시 밝기를 낮춘다 — 원본 에셋은 보존, 여기서만 RGB × DIM. user override
/// (students_dir/schale-classroom.png) 우선, 없으면 include_bytes 번들.
pub(crate) fn schale_classroom_rgba() -> Option<(Vec<u8>, u32, u32)> {
    const DIM: f32 = 0.40;
    let mut img = user_asset_rgba("schale-classroom.png")
        .and_then(|(rgba, w, h)| image::RgbaImage::from_raw(w, h, rgba))
        .or_else(|| {
            image::load_from_memory(include_bytes!("../assets/schale-classroom.png"))
                .ok()
                .map(|i| i.to_rgba8())
        })?;
    for px in img.pixels_mut() {
        px[0] = (px[0] as f32 * DIM) as u8;
        px[1] = (px[1] as f32 * DIM) as u8;
        px[2] = (px[2] as f32 * DIM) as u8;
    }
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}


#[cfg(test)]
mod student_asset_tests {
    use super::*;

    /// 번들 내장 프레임이 **모든 학생 × 모든 모션**에서 실제로 디코딩되는지.
    ///
    /// `student_sprite_frames` 는 프레임 크기가 하나라도 다르거나 PNG 하나가
    /// 안 풀리면 그 모션을 통째로 `None` 으로 돌려주고, 호출측은 업로드를 건너뛴
    /// 뒤 없는 키로 `queue_image_above` 를 부른다 — **아무것도 안 그려지고 에러도
    /// 없다**. 그래서 "프사(정적 로더)는 뜨는데 애니만 안 뜬다" 가 되면 원인을
    /// 밖에서 가릴 수 없다(2026-08-05 거노 신고 추적에 하루가 들었다).
    #[test]
    fn bundled_sprite_frames_decode_for_every_student_and_motion() {
        let mut checked = 0;
        let mut with_art = 0;
        // 여기는 활성 로스터가 아니라 코드젠 상수를 직접 쓴다 — 이건
        // **번들** 자산이 번들 로스터 전원을 덮는지 보는 테스트다. 런타임 로스터로
        // 바꾸면 테마를 깐 기계에서 그 테마를 검사하게 되고, 번들 구멍은 못 잡으면서
        // 남의 테마 때문에 실패한다.
        for (_, slug) in crate::theme::CHARACTER_SLUGS {
            // 로스터(build.rs 가 characters.json 에서 굽는다)와 `student_sprite_png` 의
            // 슬러그 목록은 **정본이 둘**이라 어긋나도 오류가 안 난다 — 빠진 학생은
            // 그냥 그림 없는 학생이 되고 화면에서만 조용히 사라진다. 실제로 에셋을
            // 67명분 만들어 놓고 목록에 안 넣어 앱에는 12명만 보였다(2026-08-11).
            assert!(
                student_sprite_png(slug, "idle").is_some(),
                "{slug} 가 로스터엔 있는데 student_sprite_png 목록에 없다 — 앱에서 안 보인다"
            );
            with_art += 1;
            for motion in ["idle", "wave", "cheer", "walk"] {
                let frames = student_sprite_frames(slug, motion)
                    .unwrap_or_else(|| panic!("{slug}/{motion}: 프레임이 None — 애니가 통째로 안 그려진다"));
                let want = if motion == "walk" {
                    STUDENT_WALK_FRAMES
                } else {
                    STUDENT_IDLE_FRAMES
                };
                assert_eq!(frames.len(), want, "{slug}/{motion}: 프레임 수");
                for (i, (rgba, w, h)) in frames.iter().enumerate() {
                    assert!(*w > 0 && *h > 0, "{slug}/{motion}[{i}]: 0 크기");
                    assert_eq!(
                        rgba.len(),
                        (*w as usize) * (*h as usize) * 4,
                        "{slug}/{motion}[{i}]: RGBA 길이가 w*h*4 와 안 맞는다"
                    );
                }
                checked += 1;
            }
        }
        assert!(checked >= 4, "모션을 하나도 못 셌다 — 로스터가 비었나");
        // 로스터가 통째로 비면 위 루프는 조용히 0바퀴 돌고 전부 통과한다. 실측 79명을
        // 하한으로 박아 그 침묵을 막는다.
        assert!(with_art >= 79, "아트 있는 학생이 {with_art}명뿐 — 에셋이 빠졌나");
    }

    /// 프사도 같은 계약이다 — 스프라이트만 79명이고 프사가 12명 수동 목록으로
    /// 남아, 신규 학생의 SendMessage/tell 아바타 자리가 조용히 비었다(2026-08-12).
    #[test]
    fn bundled_profile_decodes_for_every_student() {
        let mut with_art = 0;
        // 코드젠 상수 직접 사용 — 위 테스트와 같은 이유(번들끼리 대조).
        for (_, slug) in crate::theme::CHARACTER_SLUGS {
            let png = student_profile_png(slug).unwrap_or_else(|| {
                panic!("{slug} 가 로스터엔 있는데 student_profile_png 목록에 없다")
            });
            let img = image::load_from_memory(png)
                .unwrap_or_else(|e| panic!("profile/{slug}.png 디코드 실패: {e}"));
            let (w, h) = (img.width(), img.height());
            assert!(w > 0 && h > 0, "profile/{slug}.png 크기 0");
            with_art += 1;
        }
        assert!(with_art >= 79, "프사 있는 학생이 {with_art}명뿐");
    }

    /// 그림 유무를 **슬롯 세우기 전에** 가르는 계약.
    ///
    /// 이게 무너지면 화면에 구멍이 난다: 호출부가 Clawd 배너·스피너 글리프를 먼저
    /// blank 로 지우고 나서 슬롯을 세우므로, 그림이 없는데 세우면 원래 있던 것까지
    /// 지워진 채 아무것도 안 그려진다. 로스터가 12→79명이 되며 실제로 그랬다.
    ///
    /// 로스터 79명은 이제 전원 그림이 있다(2026-08-11). 그래서 여기서 거르는 대상은
    /// 로스터 **밖** 이름뿐이다 — 이름을 바꾼 세션이 그 경로로 들어온다.
    #[test]
    fn students_without_art_are_filtered_before_the_glyph_is_erased() {
        for motion in ["idle", "wave", "cheer", "walk"] {
            assert!(student_has_sprite("arisu", motion), "아리스/{motion}");
        }
        assert!(!student_has_sprite("존재하지않는슬러그", "walk"));
        assert!(!student_has_sprite("모바일", "idle"));
    }

    // 사용자 override 파일이 없으면 None → 호출측이 번들 include_bytes 로 폴백.
    #[test]
    fn user_asset_missing_falls_back() {
        let dir = std::env::temp_dir().join(format!("kt-noassets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(user_asset_rgba_in(&dir, &profile_rel("yuuka", true)).is_none());
    }

    // override 파일이 있으면 그걸 읽고, 과대 이미지는 MAX_STUDENT_EDGE 로 종횡비
    // 유지 다운스케일(640×480 → 512×384).
    #[test]
    fn user_asset_read_and_downscale() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("kt-assets-{}-{n}", std::process::id()));
        std::fs::create_dir_all(dir.join("profile")).unwrap();
        let rel = profile_rel("yuuka", true);
        image::RgbaImage::from_pixel(640, 480, image::Rgba([10, 20, 30, 255]))
            .save(dir.join(&rel))
            .unwrap();
        let (rgba, w, h) = user_asset_rgba_in(&dir, &rel).expect("override read");
        assert_eq!((w, h), (MAX_STUDENT_EDGE, 384));
        assert_eq!(rgba.len() as u32, w * h * 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // 규격 이하 이미지는 원본 크기 그대로(불필요한 리샘플 방지).
    #[test]
    fn user_asset_small_kept_verbatim() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("kt-small-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::from_pixel(96, 96, image::Rgba([1, 2, 3, 255]))
            .save(dir.join("schale-logo.png"))
            .unwrap();
        let (_, w, h) = user_asset_rgba_in(&dir, "schale-logo.png").expect("override read");
        assert_eq!((w, h), (96, 96));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// override 폴더는 모션 폴더를 먼저 보고, 없으면 폴더가 나뉘기 전의 평면
    /// 이름으로 떨어진다 — 그리고 **두 구조를 섞지 않는다**.
    ///
    /// 섞이면 폴더로 절반만 옮긴 사용자의 애니가 옛 그림과 뒤섞여 튄다. 그건
    /// all-or-nothing 규칙이 처음부터 막으려던 증상이라, 구조 사이에도 같은
    /// 규칙이 걸려야 한다.
    #[test]
    fn user_sprites_prefer_folders_but_never_mix_layouts() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("kt-layout-{}-{n}", std::process::id()));
        std::fs::create_dir_all(dir.join("walk")).unwrap();
        let put = |rel: String, v: u8| {
            image::RgbaImage::from_pixel(8, 8, image::Rgba([v, 0, 0, 255]))
                .save(dir.join(rel))
                .unwrap();
        };
        let red = |imgs: &[image::RgbaImage]| imgs[0].get_pixel(0, 0)[0];

        // 옛 평면 이름으로 한 벌(6장)을 채워 둔 기존 사용자.
        for i in 0..STUDENT_WALK_FRAMES {
            put(sprite_rel("mika", "walk", i, false), 20);
        }
        // 새 폴더로는 절반만 옮긴 상태 — 벌이 안 차므로 아직 옛 벌이 이긴다.
        for i in 0..3 {
            put(sprite_rel("mika", "walk", i, true), 90);
        }
        let got = user_sprite_images_in(&dir, "mika", "walk").expect("옛 평면 한 벌");
        assert_eq!(got.len(), STUDENT_WALK_FRAMES);
        assert_eq!(red(&got), 20, "절반만 옮긴 새 폴더가 이기면 애니가 섞인다");

        // 나머지를 마저 옮기면 그때부터 새 폴더가 이긴다.
        for i in 3..STUDENT_WALK_FRAMES {
            put(sprite_rel("mika", "walk", i, true), 90);
        }
        assert_eq!(
            red(&user_sprite_images_in(&dir, "mika", "walk").expect("새 구조 한 벌")),
            90
        );

        // 어느 구조에도 없는 모션은 None → 호출측이 번들 도트로 떨어진다.
        assert!(user_sprite_images_in(&dir, "mika", "cheer").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 파일 이름 규약 자체를 못 박는다 — 이게 흔들리면 번들·override·내보내기가
    /// 서로 다른 자리를 가리키고, 그 어긋남은 오류 없이 「그림이 안 바뀐다」로만
    /// 드러난다.
    #[test]
    fn sprite_paths_follow_the_documented_layout() {
        assert_eq!(sprite_rel("mika", "idle", 2, true), "idle/mika-2.png");
        assert_eq!(sprite_rel("mika", "idle", 2, false), "mika-2.png");
        assert_eq!(sprite_rel("mika", "walk", 5, true), "walk/mika-5.png");
        assert_eq!(sprite_rel("mika", "walk", 5, false), "mika-walk-5.png");
        assert_eq!(profile_rel("mika", true), "profile/mika.png");
        assert_eq!(profile_rel("mika", false), "mika-profile.png");
    }

    // 얼굴 크롭은 **위쪽**을 담는다 — 아래를 담으면 몸통만 남아 누구인지 사라진다.
    // 위 절반이 빨강, 아래 절반이 파랑인 판을 넣어 결과가 순수 빨강인지로 잰다.
    #[test]
    fn profile_face_crop_keeps_the_top() {
        let mut img = image::RgbaImage::from_pixel(100, 100, image::Rgba([0, 0, 255, 255]));
        for y in 0..62 {
            for x in 0..100 {
                img.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        let (rgba, w, h) = crop_profile_face(img.into_raw(), 100, 100).expect("crop");
        // 변은 원래대로 되돌아온다(큰 칸에서 업스케일로 흐려지지 않게).
        assert_eq!((w, h), (100, 100));
        let out = image::RgbaImage::from_raw(w, h, rgba).unwrap();
        // 자른 62% 안은 전부 빨강이었으므로 리샘플 뒤에도 파랑이 섞이면 안 된다.
        for p in out.pixels() {
            assert!(p[0] > 200 && p[2] < 60, "아래쪽(파랑)이 섞였다: {p:?}");
        }
    }

    // 이미 얼굴만 담긴 작은 에셋은 다시 자르지 않는다 — 두 번 자르면 눈만 남는다.
    #[test]
    fn profile_face_crop_skips_when_ratio_is_moot() {
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]));
        let raw = img.into_raw();
        let n = raw.len();
        let (rgba, w, h) = crop_profile_face(raw, 4, 4).expect("crop");
        assert_eq!((w, h, rgba.len()), (4, 4, n));
    }
}


#[cfg(test)]
mod sidebar_blink_tests {
    use super::*;

    // 걷기 스프라이트가 전 학생에게 있는지는 `student_asset_tests` 의
    // `bundled_sprite_frames_decode_for_every_student_and_motion` 이 이미, 더 깐깐하게
    // 본다(프레임 수 + RGBA 길이까지). 여기서 또 보지 않는다.

    /// 깜빡임은 켜짐·꺼짐이 **둘 다 평평해야** 깜빡임으로 읽힌다. 사인파로 돌아가면
    /// (그게 걷어낸 숨쉬기다) 6px 점에서는 그냥 흐린 점이 된다.
    #[test]
    fn blink_rests_at_both_ends() {
        assert!(blink(0.25, 1.0) > 0.99, "켜짐 구간이 평평하지 않다");
        assert!(blink(0.80, 1.0) < 0.01, "꺼짐 구간이 평평하지 않다");
        // 가장자리는 램프 — 즉각 점멸은 시야 가장자리에서 지나치게 튄다.
        let mid = blink(0.04, 1.0);
        assert!((0.1..0.9).contains(&mid), "전환이 계단이다: {mid}");
        // 주기를 넘겨도 같은 위상. 프레임 시계는 프로세스 시작부터 단조증가라
        // 몇 시간 뒤에도 같은 리듬이어야 한다.
        assert!((blink(0.25, 1.0) - blink(600.25, 1.0)).abs() < 1e-3);
    }

    /// 남이 보낸 메시지가 학생 테마로 뜨려면 발신자 이름에서 슬러그가 나와야 한다.
    /// 2026-08-04 에 이름이 세 토막(`<슬러그>-p<번호>-<접미>`)이 되면서 옛 파싱이
    /// `himari-p2` 를 내어 로스터에 못 걸렸다 — 그때부터 색도 프사도 없이 떴다.
    #[test]
    fn sender_slug_survives_three_part_agent_names() {
        assert_eq!(teammate_sender_slug("himari-p2-1uc"), Some("himari"));
        assert_eq!(teammate_sender_slug("arisu-p116-1uc"), Some("arisu"));
        // 옛 두 토막 형식도 그대로 걸려야 한다.
        assert_eq!(teammate_sender_slug("aru-9c88"), Some("aru"));
        // 한글 표시명은 로스터 정면 매칭.
        assert_eq!(teammate_sender_slug("히마리"), Some("himari"));
        // 이름을 바꾼 세션은 이름만으로 캐릭터를 알 길이 없다 — 여기서 None 이 맞고,
        // 그 복구는 소켓 pid 로 pane 을 되짚는 별도 경로가 맡는다.
        assert_eq!(teammate_sender_slug("모바일"), None);
    }

    #[test]
    fn sender_accent_picks_the_student_before_the_tag_color() {
        let himari = theme::character_accent("히마리").expect("로스터에 있어야 한다");
        assert_eq!(teammate_sender_accent("himari-p2-1uc", None), himari);
        // 학생을 못 찾을 때만 태그 색으로 떨어진다.
        assert_eq!(
            teammate_sender_accent("모바일", Some("red")),
            [224, 88, 78, 255]
        );
    }
}
