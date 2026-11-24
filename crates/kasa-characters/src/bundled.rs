//! Portraits and standing art shared by native and web clients.

struct CharacterArt {
    slug: &'static str,
    portrait: &'static [u8],
    standing: &'static [u8],
}

macro_rules! art {
    ($($slug:literal),* $(,)?) => {
        &[$(CharacterArt {
            slug: $slug,
            portrait: include_bytes!(concat!("../../../app/kasaterm/assets/students/profile/", $slug, ".png")),
            standing: include_bytes!(concat!("../../../app/kasaterm/assets/students/standing/", $slug, ".png")),
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

pub fn standing(slug: &str) -> Option<&'static [u8]> {
    CHARACTERS.iter().find(|art| art.slug == slug).map(|art| art.standing)
}
