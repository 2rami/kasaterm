# kasaterm — Windows 개발 툴체인 1회 설치와 런타임 의존성 확인.
# winget 으로 MSVC/Rust/Node/Git/Python 을 깐다. 관리자 PowerShell 에서 실행하고,
# 끝나면 새 창을 열어 PATH 를 반영한 뒤 -Check 로 확인할 것.
#
#   powershell -ExecutionPolicy Bypass -File scripts\windows\setup.ps1          # 설치
#   powershell -ExecutionPolicy Bypass -File scripts\windows\setup.ps1 -Check   # 확인만(아무것도 안 깐다)
#
# 앱 자체도 실행 중에 두 가지를 바깥에서 빌린다 — 설치본(MSI·portable)을 쓰는 기계도 마찬가지다.
#   Git for Windows 의 sh.exe — 칸 훅·`.cmd` 짝을 돌리는 POSIX sh. 앱은 Git 의 bash.exe 옆 sh.exe 를 찾는다.
#   Python 3 — 훅·kasacollab·하네스 신원 확인. 앱은 python3 → python → py 순으로 `--version` 이
#     `Python 3` 이라 답하는 첫 것을 쓴다. WindowsApps 의 python3/python 은 스토어 바로가기 껍데기라
#     실행하면 실패한다(exit 49) — 그것만 있으면 훅이 소리 없이 다 멈춘다.

param(
    [switch]$Check
)

$ErrorActionPreference = "Stop"

function Test-Toolchain {
    $problems = New-Object System.Collections.Generic.List[string]
    $rows = New-Object System.Collections.Generic.List[object]

    function Add-Row([string]$Name, [bool]$Ok, [string]$Detail) {
        $rows.Add([pscustomobject]@{ item = $Name; ok = $(if ($Ok) { "ok" } else { "MISSING" }); detail = $Detail })
        if (-not $Ok) { $problems.Add("$Name — $Detail") }
    }

    # 없는 이름을 부르면 $LASTEXITCODE 가 이전 값으로 남는다 — 먼저 찾고, 종료 코드와 출력 둘 다 본다.
    function Get-FirstLine([string]$Program, [string[]]$Arguments) {
        if (-not (Get-Command $Program -ErrorAction SilentlyContinue)) { return $null }
        $previous = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        try {
            $output = @(& $Program @Arguments 2>$null)
            $code = $LASTEXITCODE
        } catch {
            return $null
        } finally {
            $ErrorActionPreference = $previous
        }
        if ($code -eq 0 -and $output.Count -and "$($output[0])") { return "$($output[0])" }
        return $null
    }

    foreach ($tool in @(
            @{ name = "rustc"; args = @("--version") },
            @{ name = "cargo"; args = @("--version") },
            @{ name = "node"; args = @("--version") },
            @{ name = "npm.cmd"; args = @("--version") },
            @{ name = "git"; args = @("--version") })) {
        $version = Get-FirstLine $tool.name $tool.args
        Add-Row $tool.name ([bool]$version) $(if ($version) { $version } else { "not on PATH" })
    }
    if (Get-Command rustc -ErrorAction SilentlyContinue) {
        $rustHost = (& rustc -vV | Select-String '^host: ').Line
        Add-Row "rustc host" ([bool]$rustHost) "$rustHost"
    }

    # 앱의 git_bash_path() 와 같은 후보, 같은 순서.
    $bash = @(
        (Join-Path $env:ProgramFiles "Git\bin\bash.exe"),
        (Join-Path $env:ProgramFiles "Git\usr\bin\bash.exe"),
        (Join-Path ${env:ProgramFiles(x86)} "Git\bin\bash.exe")
    ) | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
    Add-Row "Git bash.exe" ([bool]$bash) $(if ($bash) { $bash } else { "Git for Windows not found under Program Files" })
    $sh = if ($bash) { Join-Path (Split-Path $bash) "sh.exe" } else { $null }
    Add-Row "Git sh.exe (hooks)" ($sh -and (Test-Path -LiteralPath $sh)) $(if ($sh) { $sh } else { "needs Git bash.exe first" })

    # 앱의 python3_program() 과 같은 판정 — 이름 순서대로 `--version` 이 `Python 3` 인 첫 것.
    $python = $null
    foreach ($name in @("python3", "python", "py")) {
        $command = Get-Command $name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $command) { continue }
        $version = Get-FirstLine $command.Source @("--version")
        if ($version -like "Python 3*") {
            $python = [pscustomobject]@{ name = $name; path = $command.Source; version = $version }
            break
        }
        if ($command.Source -match '\\WindowsApps\\') {
            Add-Row "$name (store stub)" $true "$($command.Source) — skipped, not a real interpreter"
        }
    }
    Add-Row "Python 3 (hooks, identity)" ([bool]$python) $(if ($python) { "$($python.name) → $($python.path) ($($python.version))" } else { "no real Python 3 on PATH (WindowsApps stubs do not count)" })
    if ($python) {
        $minor = [regex]::Match($python.version, '^Python 3\.(\d+)').Groups[1].Value
        Add-Row "Python >= 3.11 (check-workspace.py)" ([int]$minor -ge 11) $python.version
    }

    $rows | Format-Table -AutoSize | Out-String | Write-Host
    if ($problems.Count) {
        throw "missing: $($problems -join '; ')"
    }
    Write-Host "Windows toolchain and runtime dependencies look complete."
}

if ($Check) {
    Test-Toolchain
    return
}

Write-Host "== kasaterm Windows 툴체인 설치 =="

if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
    throw "winget 이 없다. Microsoft Store 의 '앱 설치 관리자'를 먼저 설치할 것."
}

function Install-Pkg($id, [string[]]$extra) {
    Write-Host "-- $id"
    winget install --id $id -e --accept-source-agreements --accept-package-agreements @extra
    # 이미 깔린 패키지는 winget 이 0 이 아닌 코드로 끝낼 수 있다 — 판정은 마지막 -Check 가 한다.
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "winget $id exited with $LASTEXITCODE (already installed or failed — see -Check)"
    }
}

# MSVC 링커(link.exe) + Windows SDK(rc.exe — winresource 가 app.ico 를 exe 에 임베드).
# VCTools 워크로드가 ARM64/x64 컴파일러·SDK 를 함께 설치한다.
Install-Pkg "Microsoft.VisualStudio.2022.BuildTools" @(
    "--override", "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
)

Install-Pkg "Rustlang.Rustup"       @()   # host = 머신 아키텍처(ARM VM → aarch64-pc-windows-msvc)
Install-Pkg "OpenJS.NodeJS.LTS"     @()   # arona-ui(웹뷰) 빌드
Install-Pkg "Git.Git"               @()   # clone · build.rs 의 git rev 스탬프 · 칸 훅의 sh.exe
Install-Pkg "Python.Python.3.12"    @()   # 칸 훅·kasacollab·하네스 신원 확인 · check-workspace.py

Write-Host ""
Write-Host "설치 끝. 새 PowerShell 창에서 확인:"
Write-Host "  powershell -ExecutionPolicy Bypass -File scripts\windows\setup.ps1 -Check"
Write-Host "Python 이 스토어 바로가기로 잡히면 설정 → 앱 → 앱 실행 별칭에서 python.exe·python3.exe 를 끈다."
Write-Host "그다음  scripts\windows\build-run.ps1  실행."
