# kasaterm — Windows 11 (ARM64) 개발 툴체인 1회 설치.
# 경로 B(VM 안 네이티브 빌드)용. winget 으로 MSVC/Rust/Node/Git 를 깐다.
# 관리자 PowerShell 에서 실행하고, 끝나면 새 창을 열어 PATH 를 반영할 것.
#
#   powershell -ExecutionPolicy Bypass -File scripts\windows\setup.ps1

$ErrorActionPreference = "Stop"
Write-Host "== kasaterm Windows 툴체인 설치 =="

if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
    throw "winget 이 없다. Microsoft Store 의 '앱 설치 관리자'를 먼저 설치할 것."
}

function Install-Pkg($id, [string[]]$extra) {
    Write-Host "-- $id"
    winget install --id $id -e --accept-source-agreements --accept-package-agreements @extra
}

# MSVC 링커(link.exe) + Windows SDK(rc.exe — winresource 가 app.ico 를 exe 에 임베드).
# VCTools 워크로드가 ARM64/x64 컴파일러·SDK 를 함께 설치한다.
Install-Pkg "Microsoft.VisualStudio.2022.BuildTools" @(
    "--override", "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
)

Install-Pkg "Rustlang.Rustup"   @()   # host = 머신 아키텍처(ARM VM → aarch64-pc-windows-msvc)
Install-Pkg "OpenJS.NodeJS.LTS" @()   # arona-ui(웹뷰) 빌드
Install-Pkg "Git.Git"           @()   # clone + build.rs 의 git rev 스탬프

Write-Host ""
Write-Host "설치 끝. 새 PowerShell 창에서 확인:"
Write-Host "  rustc -vV        # host: aarch64-pc-windows-msvc 여야 네이티브"
Write-Host "  node -v; git -v"
Write-Host "그다음  scripts\windows\build-run.ps1  실행."
