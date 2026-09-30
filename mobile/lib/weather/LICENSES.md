# 날씨(폰) — 쓴 것과 라이선스

| 무엇 | 가져온 것 | 방식 | 라이선스 |
| --- | --- | --- | --- |
| 카드 유리 물방울 | [raindrop-fx](https://github.com/SardineFish/raindrop-fx) (SardineFish) — 맺힘·합쳐짐·미끄러짐·자국 흐름, compose 셰이더, 법선 스프라이트 | 흐름을 카드 단위로 새로 짬(`sim.dart`), 합성은 `shaders/weather_glass.frag`, 스프라이트는 원본 저장소 것(`assets/weather/raindrop_normal.LICENSE.txt`) | MIT |
| 빗줄기 | 「Rain and Snow with Parallax Effect」 (Brian Smith, godotshaders.com) | 기법만 옮겨 새로 짬(`shaders/weather_rain.frag`) | MIT |
| 단추 물방울 | 데스크톱 `app/kasaterm/src/weather/shaders/buttons.wgsl`(이 레포) — 매끈한 최솟값은 Inigo Quilez 의 다항 smooth-min 공식 | GLSL 로 옮기며 뒤를 비추지 않는 막으로(`shaders/weather_buttons.frag`) | 레포 라이선스 |
| 고인 물·닦기·파문 고리·바닥 파문 | 카사텀 자체 | — | 레포 라이선스 |

시제품(`draft/liquid-rain-mobile`)에서 쓴 gooey·liquid_refraction_surface 는 앱에 넣지 않았다 — 앱의 단추는 A 판 단추
그대로 두고 그 위에 물방울을 얹어야 해서(gooey 는 단추를 제 방울로 바꾼다), 카드는 투명한 A 판 위에서 바탕을 검게
칠하고 글을 7pt 넘게 미는 파문을 쓸 수 없어서(liquid_refraction_surface 는 불투명 출력).

쓰지 않은 것: Heartfelt(Shadertoy, 비영리 한정)와 그 파생, flutter_shader_kit 의 WeatherRainLayer, GPL/LGPL 코드.
