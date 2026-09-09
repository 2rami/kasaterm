# Viewer design references

이 문서는 독립 Markdown Viewer의 읽기 화면을 다듬을 때 참고한 공개 UI만 기록한다. 구현 코드는 복사하지 않았고 새 의존성도 들이지 않는다.

## MarkText

- 출처: [공식 저장소](https://github.com/marktext/marktext), [공식 화면](https://github.com/marktext/marktext/raw/develop/docs/assets/marktext.png)
- 라이선스: MIT
- 실제 관찰: WYSIWYG 본문 위에 편집 도구를 모으고, 왼쪽 한 칸에서 파일·검색·헤딩 목차를 전환한다. 파일명과 단어 수는 본문 밖의 조용한 정보로 남긴다.
- 선택한 패턴: 왼쪽 문서 개요와 오른쪽 문서명/상태행·평평한 도구행의 비중, 중성 다크 표면 계층을 Viewer의 기준으로 삼는다. WYSIWYG 구조와 단어 수 기능은 가져오지 않는다.

## Inlyne

- 출처: [공식 저장소](https://github.com/inlyne-project/inlyne), [공식 README 화면](https://raw.githubusercontent.com/trimental/inlyne/v0.4/assets/img/example.png)
- 라이선스: MIT
- 실제 관찰: 앱 장식은 최소화하고, 문서를 중앙의 한 읽기 열과 넓은 여백으로 보여 준다. 오른쪽에는 얇은 스크롤 표시만 둔다.
- 선택한 패턴: 보기 모드의 도구 밀도를 낮추고 읽기 흐름과 스크롤 위치를 먼저 보이게 한다.

## ghostwriter

- 출처: [공식 저장소](https://github.com/KDE/ghostwriter), [공식 사이트](https://ghostwriter.kde.org/), [공식 목차 화면](https://ghostwriter.kde.org/images/outline.png)
- 라이선스: GNU GPL v3.0. 저장소 안의 서드파티 구성요소는 각각 호환 라이선스를 따른다.
- 실제 관찰: 접을 수 있는 계층형 목차가 현재 섹션과 마우스 강조를 서로 다른 색으로 구분한다. 본문과 별도로 단어 수, 집중 모드, 문서 통계를 제공한다.
- 선택한 패턴: 목차는 기본 접힘으로 두고, 펼쳤을 때 현재 섹션을 강조한다. GPL 코드는 사용하지 않고 배치 원칙만 참고한다.

## 적용 계약

- 현재 Markdown 렌더러는 이미 본문 폭을 `46em`으로 제한한다. 전폭 본문을 원인으로 기록하거나 같은 제한을 중복 구현하지 않는다.
- 개선 대상은 읽기 열 자체가 아니라 그 주변이다. 넓은 창의 여백, 보기 모드의 조용한 크롬, 접히는 목차, 현재 위치, 문서 정보의 우선순위를 다듬는다.
- 넓은 새 Viewer는 목차를 22–24% rail로 열고 얇은 선으로 본문과 나눈다. 좁은 창에서는 접힌 상태로 시작하고 사용자가 열었을 때만 drawer로 겹친다.
- 문서명·저장 상태는 42px 첫 행, 기존 보기/편집·찾기·목차·줄바꿈·확대/축소는 40px 도구행에 둔다. 기본 버튼은 평평하고 활성·호버만 표면을 받는다.
- 보기·편집 선택기는 모드가 바뀌어도 같은 위치와 폭을 유지한다. 검색은 입력·가운데 정렬한 결과 수·이동·닫기를 한 간격 규칙으로 묶고, 저장 완료는 버튼 대신 읽기 쉬운 낮은 상태로 표시한다.
- 목차는 기존 Markdown AST의 실제 Heading과 블록 위치만 사용한다. 열고 닫거나 항목을 이동해도 현재 문맥과 46em 읽기 열을 보존한다.
- 목차의 현재 heading은 기존 강조색의 얇은 왼쪽 표식으로 hover와 구분한다.
- Viewer 프로세스만 MarkText 화면에 가까운 중성 다크 역할 팔레트와 차분한 오렌지 강조를 메모리에 적용한다. 문서명은 상태행 중앙, 저장 상태는 오른쪽에 두며 본체 설정과 앱 안 문서창은 바꾸지 않는다.
- 본문 스크롤은 투명한 넓은 hit 영역 안에 5px thumb를 두고 hover 7px·drag 8px로 키운다. 계산과 문서 폭은 바꾸지 않는다.
- 글꼴은 기존 proportional UI와 OpenHuman 본문을 유지하고 코드 본문만 고정폭으로 둔다. 별도 편집 엔진이나 UI 프레임워크로 교체하지 않는다.

## Windows Viewer 글꼴

- 출처: [Google Fonts `Noto Sans KR`](https://github.com/google/fonts/tree/4efc2774c63917927efe769ca845def6bd6debae/ofl/notosanskr), 라이선스: SIL Open Font License 1.1.
- 원본 `NotoSansKR[wght].ttf`를 `NotoSansKR-Variable.ttf`로 보관하며 SHA-256은 `194018e6b2b293a7964f037b25c0249ce1418bc9ab3c971060a03aa57861e252`다. OFL 파일 SHA-256은 `1c05c68c34f9708415aada51f17e1b0092d2cea709bf4a94cd38114f9e73d7d9`다.
- Viewer는 Windows에서 실행 파일 옆 `fonts` 경로의 이 원본을 읽고 weight axis 400/600을 직접 사용한다. 정적 폰트로 임의 변환하지 않으며, 코드 글꼴과 본체 kasaterm의 글꼴 선택은 바꾸지 않는다.
