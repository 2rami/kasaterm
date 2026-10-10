# kasaterm — Windows 회귀 검사. CI(.github/workflows/windows.yml)와 손으로 돌릴 때가 같은 절차를 쓴다.
#
#   scripts\windows\verify.ps1 -GenerateLockfile            # 전부(경계·check·test·굽기·smoke·조립), lock 없으면 만든다
#   scripts\windows\verify.ps1 -Stage boundaries,check      # 빠른 것만
#   scripts\windows\verify.ps1 -Stage build,smoke -SkipUi   # arona-ui dist 가 이미 있을 때
#   scripts\windows\verify.ps1 -Stage smoke -SmokeShells default,cmd   # 이 기계에 없는 셸을 명시적으로 뺄 때
#
# 맥의 `cargo check` 는 `#[cfg(windows)]` 를 한 줄도 안 보고, 맥에서 MSVC 로 크로스 체크하면 ring 이
# Windows SDK 헤더를 못 찾아 의존성에서 죽는다. 그래서 Windows 경로의 검증처는 Windows 뿐이다.
# 병렬은 기본 2 — 코어 많은 Windows 에서 rustc 를 코어 수만큼 띄우면 LLVM 이 메모리 압박으로
# 매번 다른 크레이트에서 죽는다(STATUS_ACCESS_VIOLATION·HEAP_CORRUPTION).

[CmdletBinding()]
param(
    [ValidateSet("all", "boundaries", "check", "test", "build", "smoke", "package")]
    [string[]]$Stage = @("all"),
    [string]$Repo = (Resolve-Path "$PSScriptRoot\..\.."),
    [ValidateRange(1, 64)][int]$Jobs = 2,
    [ValidateRange(1, 64)][int]$TestThreads = 2,
    [string]$ExpectedVersion,
    [switch]$SkipUi,
    [switch]$GenerateLockfile,
    [ValidateSet("default", "winps51", "cmd", "gitbash")]
    [string[]]$SmokeShells = @("default", "winps51", "cmd", "gitbash")
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$ArgumentList = @()
    )

    Write-Host "> $FilePath $($ArgumentList -join ' ')"
    & $FilePath @ArgumentList
    if ($LASTEXITCODE -ne 0) {
        throw "$FilePath $($ArgumentList -join ' ') failed with exit code $LASTEXITCODE"
    }
}

function Get-Python {
    # python3 를 앞에 두지 않는다 — Windows 의 python3 는 스토어 바로가기 껍데기일 때가 많다.
    foreach ($name in @("python", "python3")) {
        $command = Get-Command $name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($command -and $command.Source -notmatch '\\WindowsApps\\') {
            return $command.Source
        }
    }
    throw "Python 3.11+ is required (scripts/check-workspace.py uses tomllib)"
}

$repoRoot = (Resolve-Path -LiteralPath $Repo).Path
Set-Location $repoRoot

$all = @("boundaries", "check", "test", "build", "smoke", "package")
$stages = if ($Stage -contains "all") { $all } else { $all | Where-Object { $Stage -contains $_ } }
$jobsArg = "-j$Jobs"
$env:RUST_TEST_THREADS = "$TestThreads"

Write-Host "== kasaterm Windows verify: $($stages -join ', ') (jobs=$Jobs, test threads=$TestThreads) =="
Invoke-Native -FilePath "cargo.exe" -ArgumentList @("--version")
Invoke-Native -FilePath "rustc.exe" -ArgumentList @("-vV")

# Cargo.lock 은 레포에 없다(.gitignore). `--locked` 는 있는 lock 을 지키게 할 뿐이라, 없으면 무엇으로 풀지를
# 먼저 정해야 한다 — CI 는 prepare 잡이 한 번 푼 lock 을 두 잡에 똑같이 나눠 주고, 손으로는 명시적으로 만든다.
$lockPath = Join-Path $repoRoot "Cargo.lock"
if ($stages | Where-Object { @("check", "test", "build") -contains $_ }) {
    if (-not (Test-Path -LiteralPath $lockPath)) {
        if (-not $GenerateLockfile) {
            throw "Cargo.lock is missing (it is not tracked). CI supplies it from the prepare job; locally pass -GenerateLockfile."
        }
        Invoke-Native -FilePath "cargo.exe" -ArgumentList @("generate-lockfile")
    }
    Write-Host "Cargo.lock sha256 $((Get-FileHash -LiteralPath $lockPath -Algorithm SHA256).Hash.ToLowerInvariant())"
}

foreach ($current in $stages) {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    Write-Host ""
    Write-Host "== stage: $current =="
    switch ($current) {
        "boundaries" {
            $python = Get-Python
            Invoke-Native -FilePath $python -ArgumentList @("scripts/check-workspace.py")
            Invoke-Native -FilePath $python -ArgumentList @("scripts/check-workspace.py", "--self-test")
        }
        "check" {
            Invoke-Native -FilePath "cargo.exe" -ArgumentList @("check", "--locked", $jobsArg, "--all-targets")
            # 워크스페이스 전체 검사는 기능을 합쳐 버려서, 기본 기능을 끈 채 받는 가벼운 소비자
            # (kasa-collab·kasa-agents 가 받는 kasa-socket·kasa-pty)가 깨져도 위 검사는 통과한다.
            Invoke-Native -FilePath "cargo.exe" -ArgumentList @(
                "check", "--locked", $jobsArg, "--lib", "-p", "kasa-socket", "-p", "kasa-pty", "--no-default-features"
            )
            Invoke-Native -FilePath "cargo.exe" -ArgumentList @(
                "check", "--locked", $jobsArg, "--lib", "-p", "kasa-collab", "-p", "kasa-agents"
            )
        }
        "test" {
            Invoke-Native -FilePath "cargo.exe" -ArgumentList @(
                "test", "--locked", $jobsArg, "--no-fail-fast", "--", "--test-threads=$TestThreads"
            )
        }
        "build" {
            if (-not $SkipUi) {
                Push-Location (Join-Path $repoRoot "web\arona-ui")
                try {
                    Invoke-Native -FilePath "npm.cmd" -ArgumentList @("ci")
                    Invoke-Native -FilePath "npm.cmd" -ArgumentList @("run", "build")
                } finally {
                    Pop-Location
                }
            }
            if (-not (Test-Path -LiteralPath (Join-Path $repoRoot "web\arona-ui\dist\index.html"))) {
                throw "arona-ui build output is missing: web\arona-ui\dist\index.html"
            }
            # package.ps1 과 같은 두 번의 굽기 — 함께 구우면 kasa-socket 기능이 앱 쪽과 합쳐져
            # 배포되는 CLI 와 다른 CLI 를 시험하게 된다.
            Invoke-Native -FilePath "cargo.exe" -ArgumentList @("build", "--locked", $jobsArg, "--release", "-p", "kasaterm")
            Invoke-Native -FilePath "cargo.exe" -ArgumentList @(
                "build", "--locked", $jobsArg, "--release", "-p", "kasa-socket", "--bin", "kasaterm-cli"
            )
            foreach ($artifact in @("target\release\kasaterm.exe", "target\release\kasaterm-cli.exe")) {
                if (-not (Test-Path -LiteralPath (Join-Path $repoRoot $artifact))) {
                    throw "build artifact is missing: $artifact"
                }
            }
        }
        "smoke" {
            # 셸마다 앱을 따로 띄운다(칸 셸은 앱 단위 KASATERM_SHELL 로만 고른다). 고른 셸이 이 기계에 없으면
            # 건너뛰지 않고 멈춘다 — 빼려면 -SmokeShells 로 명시한다. 한 셸이 실패해도 나머지는 끝까지 돌린다.
            $known = [ordered]@{
                default = ""
                winps51 = Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe"
                cmd = Join-Path $env:SystemRoot "System32\cmd.exe"
                gitbash = Join-Path $env:ProgramFiles "Git\bin\bash.exe"
            }
            $inventory = foreach ($name in $SmokeShells) {
                $path = $known[$name]
                $present = (-not $path) -or (Test-Path -LiteralPath $path)
                $version = if ($path -and $present) { (Get-Item -LiteralPath $path).VersionInfo.ProductVersion } else { "" }
                [pscustomobject]@{ shell = $name; path = $(if ($path) { $path } else { "(app default)" }); present = $present; version = $version }
            }
            $inventory | Format-Table -AutoSize | Out-String | Write-Host
            if ($env:GITHUB_STEP_SUMMARY) {
                $rows = $inventory | ForEach-Object { "| $($_.shell) | $($_.path) | $(if ($_.present) { 'present' } else { 'MISSING' }) | $($_.version) |" }
                $table = @("### Smoke shells", "", "| shell | path | | version |", "|---|---|---|---|") + $rows + @("")
                Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Value ($table -join "`n") -Encoding utf8
            }
            $missing = @($inventory | Where-Object { -not $_.present })
            if ($missing.Count) {
                throw "required smoke shells missing: $(($missing | ForEach-Object { "$($_.shell) ($($_.path))" }) -join ', ')"
            }
            $failed = @()
            foreach ($name in $SmokeShells) {
                try {
                    & (Join-Path $PSScriptRoot "smoke.ps1") -Repo $repoRoot -Label "release-$name" -Shell $known[$name]
                } catch {
                    Write-Host "smoke release-$name failed: $($_.Exception.Message)"
                    $failed += $name
                }
            }
            if ($failed.Count) {
                throw "smoke failed for shells: $($failed -join ', ')"
            }
        }
        "package" {
            $arguments = @{ Repo = $repoRoot; SkipBuild = $true; SkipUi = $true }
            if ($ExpectedVersion) {
                $arguments["ExpectedVersion"] = $ExpectedVersion
            }
            & (Join-Path $PSScriptRoot "package.ps1") @arguments
        }
    }
    Write-Host "== stage $current passed in $([math]::Round($watch.Elapsed.TotalMinutes, 1)) min =="
}
