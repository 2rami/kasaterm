# 맥(Apple Silicon)에서 kasaterm Windows 실제 실행 테스트

목적: wgpu 렌더·한글 IME·아로나 웹뷰를 **Windows에서 직접 띄워 눈으로 확인**한다.
Apple Silicon은 x86_64 Windows를 네이티브로 못 돌린다 → Windows 11 **ARM64**를
VM으로 띄우고, 그 안에서 kasaterm를 **네이티브(aarch64-pc-windows-msvc)로 소스 빌드**한다.

CI(`.github/workflows/release.yml`)는 x86_64 MSI만 만든다. 그건 배포용이고,
여기 스크립트는 "지금 작업 중인 코드"를 VM에서 빠르게 돌려보는 용도다.

---

## 1. VM 준비

| VM | 비용 | wgpu 적합성 |
|---|---|---|
| Parallels Desktop | 유료(14일 체험) | 매끄러움. 렌더 체감 신뢰 가능 — **권장** |
| VMware Fusion | 개인 무료 | 중간 |
| UTM (QEMU) | 무료 | 가상 GPU 약함. wgpu가 느리거나 소프트웨어 fallback |

kasaterm는 wgpu 셀 렌더러라 **VM 가상 GPU 성능 = 테스트 신뢰도**다. UTM은 무료지만
스크롤·입력 지연이 VM 탓인지 코드 탓인지 구분이 안 돼 체감 판단이 오염된다.

Parallels 기준: 새 VM → "Windows 11 설치" 선택 시 ARM64 ISO를 자동으로 받아준다.

---

## 2. 소스를 VM에 넣기 (둘 중 하나)

- **git clone** (깔끔, push된 것만): VM 안에서
  ```powershell
  git clone https://github.com/2rami/kasaterm.git
  ```
  미커밋 변경은 안 들어온다. 커밋/푸시 후 VM에서 `git pull`.

- **공유폴더 → VM 로컬 복사** (미커밋 코드 테스트): Parallels가 맥 홈을 `\\Mac\Home`에
  마운트한다. `target`·`node_modules`를 빼고 VM 로컬 디스크로 복사한다(공유폴더에서
  바로 `cargo build`하면 매우 느리고 심링크·권한 문제):
  ```powershell
  robocopy \\Mac\Home\Desktop\momewomo\tmuxify C:\kasaterm /MIR /XD target node_modules .git
  ```

---

## 3. 툴체인 설치 (VM 안, 1회)

관리자 PowerShell:
```powershell
cd C:\kasaterm
powershell -ExecutionPolicy Bypass -File scripts\windows\setup.ps1
```
MSVC 빌드툴 + Rust(rustup) + Node + Git를 winget으로 깐다. 끝나면 새 창을 열고
`rustc -vV`의 host가 `aarch64-pc-windows-msvc`인지 확인(네이티브면 이게 맞다).

---

## 4. 빌드 + 실행 (반복)

```powershell
cd C:\kasaterm
scripts\windows\build-run.ps1
```
- `web/arona-ui`를 먼저 빌드해 dist를 만들고(웹뷰 콘텐츠 — MSI엔 없다),
- `cargo build --release -p kasaterm` 후,
- `KASATERM_ARONA_UI_DIR`를 잡고 exe를 실행한다.

반복 빌드에서 웹뷰를 안 고쳤으면 `-SkipUi`로 npm 단계를 건너뛴다.

---

## 5. 트러블슈팅

- **창이 안 뜨거나 wgpu 에러** — VM 가상 GPU가 DX12를 못 받는 경우. 백엔드를 강제:
  ```powershell
  scripts\windows\build-run.ps1 -SkipUi -WgpuBackend dx12   # 안 되면 vulkan, gl 순서로
  ```
- **아로나 웹뷰가 빈 화면** — `web\arona-ui\dist\index.html`이 있는지 확인. 없으면
  `-SkipUi` 없이 다시 실행해 dist를 만든다.
- **link.exe / rc.exe 못 찾음** — VS Build Tools 설치가 덜 됨. "x64 Native Tools /
  ARM64 Native Tools Command Prompt"에서 실행하거나 새 로그인으로 PATH 반영.
- **kasa-mcp에서 STATUS_ACCESS_VIOLATION** — 루트 Cargo.toml의 `opt-level=0` pin이
  이걸 막는다. 그 pin을 지우지 말 것.
- **자동 업데이트(WinSparkle)** — 실행 테스트엔 불필요해 생략했다(DLL 없으면
  `win_sparkle.rs`가 no-op으로 통과). 자동업뎃까지 보려면 CI가 받는 x64 prebuilt
  `WinSparkle.dll`을 exe 옆에 둬야 하는데, ARM 네이티브 빌드에선 맞지 않는다.

---

## 참고: 체감이 이상하면 타깃을 의심

aarch64 네이티브는 CI에서 검증된 적 없는 경로다(CI는 x86_64-msvc만). wgpu/wry/
windows-sys는 aarch64-windows를 정식 지원하지만, 특정 크레이트가 깨지면 x86_64로
폴백할 수 있다(Windows 11 ARM의 x64 에뮬레이션은 꽤 좋다):
```powershell
rustup target add x86_64-pc-windows-msvc
cargo build --release -p kasaterm --target x86_64-pc-windows-msvc
# 실행 파일은 target\x86_64-pc-windows-msvc\release\kasaterm.exe
```
