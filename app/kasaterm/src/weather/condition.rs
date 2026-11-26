use serde::{Deserialize, Serialize};

macro_rules! conditions {
    (Rain => ($rain_label:literal, $rain_material:literal, $rain_variant:literal),
     $($name:ident => ($label:literal, $material:literal, $variant:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub(crate) enum WeatherCondition { $($name,)+ #[serde(other)] Rain }

        impl WeatherCondition {
            pub(crate) const ALL: &[Self] = &[Self::Rain, $(Self::$name,)+];

            pub(crate) fn label(self) -> &'static str {
                match self { Self::Rain => $rain_label, $(Self::$name => $label,)+ }
            }

            pub(crate) fn material(self) -> (f32, f32) {
                match self { Self::Rain => ($rain_material, $rain_variant), $(Self::$name => ($material, $variant),)+ }
            }
        }
    };
}

conditions! {
    Rain => ("빗줄기와 물보라", 0.0, 0.0),
    Drizzle => ("이슬비", 0.0, 1.0),
    HeavyRain => ("폭우", 0.0, 2.0),
    SunShowers => ("여우비", 0.0, 3.0),
    FreezingDrizzle => ("어는 이슬비", 0.0, 4.0),
    FreezingRain => ("어는 비", 0.0, 5.0),
    NightDrizzle => ("밤 이슬비", 0.0, 6.0),
    Clear => ("맑음", 1.0, 0.0),
    MostlyClear => ("대체로 맑음", 1.0, 1.0),
    PartlyCloudy => ("구름 조금", 1.0, 2.0),
    MostlyCloudy => ("구름 많음", 1.0, 3.0),
    Cloudy => ("흐림", 1.0, 4.0),
    ClearNight => ("맑은 밤", 1.0, 5.0),
    MostlyClearNight => ("대체로 맑은 밤", 1.0, 6.0),
    PartlyCloudyNight => ("구름 낀 밤", 1.0, 7.0),
    Sunrise => ("일출", 1.0, 8.0),
    Sunset => ("일몰", 1.0, 9.0),
    Foggy => ("안개", 2.0, 0.0),
    Haze => ("연무", 2.0, 1.0),
    Smoky => ("연기", 2.0, 2.0),
    BlowingDust => ("먼지바람", 2.0, 3.0),
    Breezy => ("산들바람", 3.0, 0.0),
    Windy => ("강풍", 3.0, 1.0),
    Flurries => ("날리는 눈", 4.0, 0.0),
    Snow => ("눈", 4.0, 1.0),
    HeavySnow => ("폭설", 4.0, 2.0),
    SunFlurries => ("햇눈", 4.0, 3.0),
    BlowingSnow => ("날림눈", 4.0, 4.0),
    Blizzard => ("눈보라", 4.0, 5.0),
    Sleet => ("얼음싸라기", 4.0, 6.0),
    WintryMix => ("진눈깨비", 4.0, 7.0),
    Hail => ("우박", 4.0, 8.0),
    IsolatedThunderstorms => ("국지성 뇌우", 5.0, 0.0),
    ScatteredThunderstorms => ("산발적 뇌우", 5.0, 1.0),
    Thunderstorms => ("뇌우", 5.0, 2.0),
    StrongStorms => ("강한 뇌우", 5.0, 3.0),
    Hot => ("폭염", 6.0, 0.0),
    Frigid => ("혹한", 6.0, 1.0),
    TropicalStorm => ("열대 폭풍", 7.0, 0.0),
    Hurricane => ("허리케인", 7.0, 1.0),
    Tornado => ("토네이도", 7.0, 2.0),
}

impl Default for WeatherCondition {
    fn default() -> Self { Self::Rain }
}

impl WeatherCondition {
    pub(crate) fn is_rain(&self) -> bool { *self == Self::Rain }
}
