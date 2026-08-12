# kasaterm — VM(Windows 11 ARM) 안에서 네이티브 빌드 + 실행.
# 레포 루트 기준으로 동작한다. arona 웹뷰까지 뜨게 하려면 dist 가 있어야 해서
# npm 빌드를 먼저 돌린 뒤 cargo build 한다.
#
#   scripts\windows\build-run.ps1                 # 전체(웹뷰 재빌드 + 빌드 + 실행)
#   scripts\windows\build-run.ps1 -SkipUi         # dist 이미 있음, 러스트만
#   scripts\windows\build-run.ps1 -WgpuBackend dx12   # 가상 GPU 문제 시 백엔드 강제

param(
    [string]$Repo = (Resolve-Path "$PSScriptRoot\..\.."),
    [switch]$SkipUi,
    [string]$WgpuBackend   # dx12 / vulkan / gl
)

$ErrorActionPreference = "Stop"
Set-Location $Repo
Write-Host "== repo: $Repo =="

# 1) arona-ui dist — 웹뷰 콘텐츠. MSI 는 이걸 번들하지 않으므로 소스에서 직접
#    빌드해 두면 kasaterm 이 dev 폴백 경로로 서빙한다(kasa-mcp/http.rs arona_ui_root).
if (-not $SkipUi) {
    Write-Host "-- arona-ui 빌드"
    Push-Location web\arona-ui
    if (Test-Path package-lock.json) { npm ci } else { npm install }
    npm run build
    Pop-Location
}
if (-not (Test-Path "web\arona-ui\dist\index.html")) {
    Write-Warning "arona-ui dist 없음 — god 모드 웹뷰가 빈 화면일 수 있다(-SkipUi 를 뺐는지 확인)."
}

# 2) 네이티브 빌드. 루트 Cargo.toml 이 kasa-mcp 를 opt-level=0 으로 pin 해 둬서
#    Windows LLVM 의 STATUS_ACCESS_VIOLATION 을 피한다 — 그대로 둘 것.
Write-Host "-- cargo build --release -p kasaterm -p kasa-socket"
cargo build --release -p kasaterm -p kasa-socket

$exe = Join-Path $Repo "target\release\kasaterm.exe"
$cli = Join-Path $Repo "target\release\kasaterm-cli.exe"
if (-not (Test-Path $cli)) { throw "missing build artifact: $cli" }
if (-not (Test-Path $exe)) { throw "빌드 산출물 없음: $exe" }

# 3) 실행. 웹뷰 dist 를 명시 지정(폴백에 의존하지 않게), 필요 시 wgpu 백엔드 강제.
$env:KASATERM_ARONA_UI_DIR = Join-Path $Repo "web\arona-ui\dist"
if ($WgpuBackend) {
    $env:WGPU_BACKEND = $WgpuBackend
    Write-Host "-- WGPU_BACKEND=$WgpuBackend"
}

Write-Host "-- 실행: $exe"
& $exe
