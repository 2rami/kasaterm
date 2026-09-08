# Viewer design references

이 문서는 독립 Markdown Viewer의 읽기 화면을 다듬을 때 참고한 공개 UI만 기록한다. 구현 코드는 복사하지 않았고 새 의존성도 들이지 않는다.

## MarkText

- 출처: [공식 저장소](https://github.com/marktext/marktext), [공식 화면](https://github.com/marktext/marktext/raw/develop/docs/assets/marktext.png)
- 라이선스: MIT
- 실제 관찰: WYSIWYG 본문 위에 편집 도구를 모으고, 왼쪽 한 칸에서 파일·검색·헤딩 목차를 전환한다. 파일명과 단어 수는 본문 밖의 조용한 정보로 남긴다.
- 선택한 패턴: 편집 모드에서만 필요한 도구를 드러내고, 파일명·저장 상태·단어 수를 문서 정보로 묶는다. WYSIWYG 구조 자체는 가져오지 않는다.

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
- 좁은 창에서는 목차와 부가 정보를 접고 본문·보기/편집·찾기·저장만 보존한다.
- 현재 OpenHuman 계열 타이포와 2행 도구막대를 바탕으로 점진 개선한다. 별도 편집 엔진이나 UI 프레임워크로 교체하지 않는다.
- 읽기 모드는 파일·저장 상태, 보기/편집, 찾기, 접힌 목차만 한 줄에 남긴다. 줄바꿈과 확대/축소는 편집 도구줄에 유지한다.
- 목차는 기존 Markdown AST의 실제 Heading과 블록 위치만 사용한다. 펼쳐도 본문 폭과 46em 읽기 열을 바꾸지 않는 오버레이다.
- 색·곡률·글꼴은 기존 `theme` 역할 토큰과 proportional UI 글꼴을 그대로 쓴다. 코드 본문 이외에 고정폭 글꼴을 장식으로 쓰지 않는다.
