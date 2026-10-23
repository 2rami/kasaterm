# Rust 규칙

[Google Rust 스타일 가이드](https://google.github.io/styleguide/rust)에서 이 저장소에 맞는 것만 골랐다.
가이드도 앱 코드는 넓은 재사용보다 단순하고 직접적인 구현을 먼저 본다.

검사는 `cargo clippy --all-targets` 다. 설정은 루트 `Cargo.toml` 의 `[workspace.lints.clippy]` 와
`clippy.toml` 이고, 제품 크레이트는 `[lints] workspace = true` 로 받는다. 아래 규칙 넷에 걸리는 경고는 0으로 둔다.

## clippy 가 지키는 것

- **별표 import 는 부모를 받는 `use super::*` 만 쓴다**(`wildcard_imports`, `clippy.toml` 의 `allowed-wildcard-imports`).
  - App 메서드를 기능별 파일로 나눈 모듈과 시험 모듈이 이 꼴이다.
  - 형제 모듈·크레이트 루트(`crate::*`)·다른 크레이트는 쓰는 이름만 가져온다. 별표면 이름이 어디서 왔는지 grep 으로 못 찾는다.
  - 옛 경로를 지키려고 다시 내보내는 `pub(crate) use …::*` 는 예외다(`render.rs`).
- **빌려 받는 인자는 그릇 대신 내용으로 받는다**(`ref_option`·`ptr_arg`·`borrowed_box`).
  `&Option<T>` 는 `Option<&T>`, `&Vec<T>` 는 `&[T]`, `&String` 은 `&str`, `&Box<T>` 는 `&T` 로 받는다.
  부르는 쪽이 어떤 꼴로 들고 있든 복사 없이 넘긴다.
- 버그를 잡는 clippy 기본 묶음(correctness·suspicious)은 켜 둔다. style·complexity·perf 묶음은 아직 정리하지 않아 끈다.

## 손으로 지키는 것

새 코드부터 지키고, 있는 코드는 손댈 때 고친다.

- 부르는 자리에서 뜻이 안 보이는 bool 인자는 enum 으로 바꾼다. bool 을 넷 이상 받는 함수는 새로 만들지 않는다.
- 네트워크·디스크를 기다리는 동안 공유 락을 쥐지 않는다. 요청은 락 밖에서 하고 결과만 짧게 넣는다. UI 스레드는
  `try_lock()` 으로 읽는다. 2026-10 미러 멈춤은 정보 칸 폴링이 원격 요청(최대 10초) 동안 피드 락을 쥐어 생겼다.
- `Arc<Mutex<…>>` 를 새로 늘리기 전에, 한 곳이 데이터를 쥐고 결과를 채널로 넘기는 길을 먼저 본다.
- 실패는 꼴을 가른다. 값이 없을 뿐이면 `Option`, 부르는 쪽이 까닭을 알아야 다시 할 수 있으면 `Result`,
  고쳐야 할 버그면 `panic!` 이다.

## 따르지 않는 것

- 자동 포맷(`cargo fmt`)은 돌리지 않는다. 저장소 전체가 한 번에 바뀌어 공유 작업 트리의 다른 변경과 부딪친다.
- 「하위 모듈이 있는 모듈 파일에는 선언·다시 내보내기만 둔다」는 따르지 않는다. App 메서드를 기능별 파일로 나누는
  지금 구조(`skills/kasapane/references/develop.md`)를 유지한다.
- Chromium 전용 규칙(`async` 금지, 작은 크레이트 강제 등)은 해당하지 않는다.
