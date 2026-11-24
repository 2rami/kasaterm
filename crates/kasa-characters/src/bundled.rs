//! Portraits and sprite frames shared by native and web clients.

struct CharacterArt {
    slug: &'static str,
    portrait: &'static [u8],
    idle: &'static [&'static [u8]],
    walk: &'static [&'static [u8]],
    wave: &'static [&'static [u8]],
    cheer: &'static [&'static [u8]],
    idle_gif: &'static [u8],
}

macro_rules! frames {
    ($slug:literal, $motion:literal, $($index:literal),+) => {
        &[$(include_bytes!(concat!("../../../app/kasaterm/assets/students/", $motion, "/", $slug, "-", $index, ".png"))),+]
    };
}

macro_rules! art {
    ($($slug:literal),* $(,)?) => {
        &[$(CharacterArt {
            slug: $slug,
            portrait: include_bytes!(concat!("../../../app/kasaterm/assets/students/profile/", $slug, ".png")),
            idle: frames!($slug, "idle", 0, 1, 2, 3),
            walk: frames!($slug, "walk", 0, 1, 2, 3, 4, 5),
            wave: frames!($slug, "wave", 0, 1, 2, 3),
            cheer: frames!($slug, "cheer", 0, 1, 2, 3),
            idle_gif: include_bytes!(concat!("../../../app/kasaterm/assets/students/gif/", $slug, ".gif")),
        }),*]
    };
}

static CHARACTERS: &[CharacterArt] = art![
    "kasa_tsuyu",
    "kasa_ame",
    "kasa_shizuku",
    "kasa_kiri",
    "kasa_nagi",
    "kasa_kosame",
    "kasa_kumo",
    "kasa_niji",
    "kasa_kaze",
    "kasa_yoake",
    "kasa_asahi",
    "kasa_komorebi",
    "kasa_minamo",
    "kasa_shio",
    "kasa_mizube",
    "kasa_taki",
    "kasa_fune",
    "kasa_hasu",
    "kasa_shirayuki",
    "kasa_fuyu",
    "kasa_koori",
    "kasa_mizore",
    "kasa_shimo",
    "kasa_kogarashi",
    "kasa_sumi",
    "kasa_ito",
    "kasa_kinu",
    "kasa_mado",
    "kasa_yoi",
    "kasa_suzu",
];

pub fn portrait(slug: &str) -> Option<&'static [u8]> {
    CHARACTERS.iter().find(|art| art.slug == slug).map(|art| art.portrait)
}

pub fn sprite_frames(slug: &str, motion: &str) -> Option<&'static [&'static [u8]]> {
    let art = CHARACTERS.iter().find(|art| art.slug == slug)?;
    match motion {
        "idle" => Some(art.idle),
        "walk" => Some(art.walk),
        "wave" => Some(art.wave),
        "cheer" => Some(art.cheer),
        _ => None,
    }
}

pub fn idle_gif(slug: &str) -> Option<&'static [u8]> {
    CHARACTERS.iter().find(|art| art.slug == slug).map(|art| art.idle_gif)
}
