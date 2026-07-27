# claude-mark.json

Anthropic **공식** 로티다. 직접 만든 게 아니라 anthropic.com 이 자기 사이트에서
실제로 돌리고 있는 프로덕션 에셋을 그대로 가져왔다.

| | |
|---|---|
| 원본 URL | `https://cdn.prod.website-files.com/6889473510b50328dbb70ae6/69cc1e56b4ec3cf96f0db5e0_lottie-star.json` |
| 원본 파일명 | `lottie-star.json` |
| 내려받은 날 | 2026-07-28 |
| 규격 | bodymovin v5.12.1 · 100×100 · 30fps · 108프레임 |
| 구성 | 레이어 둘 (`lottie_star-Mark`, `lottie_star-Shape`), 순수 벡터 — 래스터 0 |

Anthropic 이 **배포용으로 내놓는** 브랜드킷([press-kit](https://www.anthropic.com/press-kit))
에는 SVG·PNG·JPG·PDF 뿐이고 모션 에셋이 없다. 로티는 자사 웹사이트 프로덕션
에셋으로만 존재하며, 이 파일이 그중 Claude 마크에 해당하는 것이다. 로더는
저쪽도 `lottie-web` / `@dotlottie/player-component` 를 쓴다.

LottieFiles·IconScout 의 "Claude" 로티는 **전부 서드파티** 업로드다(Anthropic
공식 계정 없음, 상당수 유료). 상표 라이선스가 깨끗하지 않으니 쓰지 않는다.

## 다룰 때

- **파일을 손으로 고치지 마라.** 갱신이 필요하면 위 URL 에서 다시 받아
  `json.dumps(..., separators=(',',':'))` 로 미니파이만 해서 덮어쓴다(레포에
  들어온 것도 그 처리만 거쳤다 — 원본이 이미 미니파이돼 있어 바이트 수는 같다).
- 상표: Claude 세션을 가리키는 표식으로 쓰는 건 지명적 사용이라 문제없다.
  kasaterm 자체 브랜딩(앱 아이콘·스플래시)에는 쓰지 않는다.
- 같은 출처의 Claude Spark SVG 는 `../icons/claude.svg` 에 있다(프레스킷 원본).

`preview.html` 은 이 json 을 `fetch` 해서 눈으로 확인하는 용도다. `file://` 로는
CORS 에 막히니 이 폴더에서 `python3 -m http.server` 를 띄우고 열어야 한다.
