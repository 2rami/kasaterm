# character-src — SCHALE OS 캐릭터 에셋 생성 작업방

아로나 UI 학생 초상(`public/assets/char-*.png`)의 **생성 원본(누끼 전 흰배경)** 보관소.
완성 누끼본만 `public/assets/`로 승격한다.

## 생성 도구 — codex 내장 `$imagegen` (gpt-image-2)

별도 플러그인·OpenAI API 키 없이 **codex CLI(0.117+)** 가 ChatGPT 구독 quota로 그린다.
상세: 글로벌 메모리 `reference_codex_imagegen`.

```bash
# 화풍 통일: 기존 char-arona.png 를 reference 로 물려 같은 치비 픽셀 화풍 유지
codex exec --skip-git-repo-check \
  "Use \$imagegen with the attached reference image. Keep the EXACT same art style \
   (chibi 2-head SD, soft pixel shading, full body, halo, white background). \
   ONLY change the character to <학생명> from Blue Archive: <외모 디테일>. \
   Same canvas/framing. Output PNG." \
  -i public/assets/char-arona.png        # ← -i 는 반드시 프롬프트 뒤 (가변인자라 순서 중요)
```

- 출력: `~/.codex/generated_images/<uuid>/ig_*.png` (codex sandbox read-only라 cp 막힘 → 직접 복사)
- 흰배경 → 누끼는 `/tmp/nukki.py`(PIL 흰배경 투명+crop) 패턴 그대로
- gpt-image-2 는 `background=transparent` 미지원 → 흰/크로마 배경 생성 후 누끼

## 상태

| 학생 | raw | 누끼본(public/assets) |
|---|---|---|
| 아로나 | (기존) | char-arona.png ✅ |
| 유우카 | (기존) | char-yuuka.png ✅ |
| 시로코 | shiroko-raw.png ✅ | 누끼 대기 |
| 아리스 | — | — |
| 호시노 | — | — |
| 코하루 | — | — |

`_rejected/` = 화풍 미채택(반신 리얼 등) 참고 보관.
