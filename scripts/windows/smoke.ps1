# kasaterm — 격리 실행 smoke. 굽힌 앱을 실제로 띄워 창(GPU)·ConPTY 칸·named pipe 소켓·HTTP 를
# 한 번씩 지나가 보고, 스스로 끝나게 둔 뒤 사용자 상태를 건드리지 않았는지 확인한다.
#
#   scripts\windows\smoke.ps1                                   # target\release 를 띄운다
#   scripts\windows\smoke.ps1 -PortableZip dist\kasaterm-v…-portable.zip -Label portable
#
# 격리 키는 docs/verify-app.md 의 부팅 줄과 같다. 거기에 HOME·USERPROFILE·APPDATA·LOCALAPPDATA·TEMP 를
# 이번 실행 폴더로 옮긴다 — Windows 의 `~/.config/kasaterm` 은 HOME→USERPROFILE 순으로 풀리고,
# 웹뷰·셸이 쓰는 자리는 APPDATA·LOCALAPPDATA 다. env 로 못 가르는 사용자 레지스트리(WinSparkle 이 설정을
# 적는 HKCU\Software\momewomo)는 실행 전후를 대조한다. 거둘 때는 이번에 띄운 PID 와 그 자손만 끈다.
# 결과는 -ArtifactDirectory(기본 target\windows-smoke) 밑에 실행마다 새 폴더로 쌓는다 — 있던 것은 안 지운다.

[CmdletBinding()]
param(
    [string]$Repo = (Resolve-Path "$PSScriptRoot\..\.."),
    [string]$AppDirectory,
    [string]$PortableZip,
    [string]$ArtifactDirectory,
    [string]$Label = "release",
    [string]$Shell,
    [ValidateRange(30, 600)][int]$AutoQuitSeconds = 90,
    [ValidateRange(10, 300)][int]$ReadyTimeoutSeconds = 60,
    [ValidateRange(1, 120)][int]$ChildGraceSeconds = 15
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

if ($Label -notmatch '^[A-Za-z0-9_-]+$') {
    throw "Label must be letters, digits, '-' or '_': $Label"
}
$repoRoot = (Resolve-Path -LiteralPath $Repo).Path
$artifactBase = if ($ArtifactDirectory) {
    [IO.Path]::GetFullPath($ArtifactDirectory)
} else {
    Join-Path $repoRoot "target\windows-smoke"
}
$runId = (Get-Date -Format "yyyyMMdd-HHmmss") + "-" + [guid]::NewGuid().ToString("N").Substring(0, 8)
$artifacts = Join-Path $artifactBase "$Label-$runId"
New-Item -ItemType Directory -Path $artifacts | Out-Null

$results = New-Object System.Collections.Generic.List[object]
$cliLog = Join-Path $artifacts "cli.log"
$cliStderr = Join-Path $artifacts "cli-last-stderr.txt"
$stdoutLog = Join-Path $artifacts "app-stdout.log"
$stderrLog = Join-Path $artifacts "app-stderr.log"
$capturePath = Join-Path $artifacts "window.png"
$userRegistry = "HKCU:\Software\momewomo"

function Add-Result {
    param([string]$Name, [bool]$Ok, [string]$Detail)
    $results.Add([pscustomobject]@{ check = $Name; ok = $Ok; detail = $Detail })
    $mark = if ($Ok) { "PASS" } else { "FAIL" }
    Write-Host "[$mark] $Name — $Detail"
}

function Assert-Check {
    param([string]$Name, [bool]$Condition, [string]$Detail)
    Add-Result -Name $Name -Ok $Condition -Detail $Detail
    if (-not $Condition) {
        throw "smoke check failed: $Name — $Detail"
    }
}

function Read-Shared {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) {
        return ""
    }
    $share = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, $share)
    try {
        $reader = New-Object IO.StreamReader($stream, [Text.Encoding]::UTF8)
        return $reader.ReadToEnd()
    } finally {
        $stream.Dispose()
    }
}

function Get-LogTail {
    $lines = (Read-Shared $stderrLog) -split "`r?`n"
    return ($lines | Select-Object -Last 40) -join "`n"
}

function Get-RegistrySnapshot {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) {
        return "<absent>"
    }
    $keys = @(Get-Item -LiteralPath $Path) + @(Get-ChildItem -LiteralPath $Path -Recurse -ErrorAction SilentlyContinue)
    $lines = foreach ($key in $keys) {
        $key.Name
        foreach ($name in ($key.GetValueNames() | Sort-Object)) {
            "  $name=$($key.GetValue($name))"
        }
    }
    return ($lines -join "`n")
}

function Set-ProcessEnvironment {
    param([System.Collections.IDictionary]$Values)
    $saved = @{}
    foreach ($key in $Values.Keys) {
        $saved[$key] = [Environment]::GetEnvironmentVariable($key, "Process")
        [Environment]::SetEnvironmentVariable($key, $Values[$key], "Process")
    }
    return $saved
}

function Restore-ProcessEnvironment {
    param([hashtable]$Saved)
    foreach ($key in $Saved.Keys) {
        [Environment]::SetEnvironmentVariable($key, $Saved[$key], "Process")
    }
}

$script:proc = $null
$script:tracked = @{}

function Update-Descendants {
    if (-not $script:proc) {
        return
    }
    $table = @(Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, CreationDate, Name, CommandLine)
    $queue = New-Object System.Collections.Generic.Queue[int]
    $seen = New-Object System.Collections.Generic.HashSet[int]
    $queue.Enqueue($script:proc.Id)
    while ($queue.Count -gt 0) {
        $parent = $queue.Dequeue()
        if (-not $seen.Add($parent)) {
            continue
        }
        foreach ($child in $table | Where-Object { $_.ParentProcessId -eq $parent }) {
            $childId = [int]$child.ProcessId
            if (-not $script:tracked.ContainsKey($childId)) {
                $script:tracked[$childId] = $child
            }
            $queue.Enqueue($childId)
        }
    }
}

function Get-LiveTracked {
    # PID 는 재사용되니 만든 시각까지 같은 것만 이번 실행의 자손으로 본다.
    foreach ($entry in @($script:tracked.GetEnumerator())) {
        $live = Get-CimInstance Win32_Process -Filter "ProcessId = $($entry.Key)" -ErrorAction SilentlyContinue
        if ($live -and $live.CreationDate -eq $entry.Value.CreationDate) {
            $entry.Value
        }
    }
}

function Stop-Tracked {
    # 이름으로 거두지 않는다 — 같은 기계의 다른 kasaterm(사람이 쓰는 앱)과 그 칸의 셸이 함께 죽는다.
    if ($script:proc -and -not $script:proc.HasExited) {
        Update-Descendants
        Stop-Process -Id $script:proc.Id -Force -ErrorAction SilentlyContinue
    }
    foreach ($child in @(Get-LiveTracked)) {
        Stop-Process -Id ([int]$child.ProcessId) -Force -ErrorAction SilentlyContinue
    }
}

function Wait-Until {
    param([string]$What, [int]$Seconds, [scriptblock]$Condition)
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        if (& $Condition) {
            return $true
        }
        if ($script:proc.HasExited) {
            throw "app exited (code $($script:proc.ExitCode)) while waiting for $What`n$(Get-LogTail)"
        }
        Update-Descendants
        Start-Sleep -Milliseconds 500
    }
    return $false
}

function Invoke-Cli {
    param([string[]]$Arguments)
    # Windows PowerShell 5.1 은 Stop 상태에서 네이티브 stderr 한 줄을 예외로 바꾼다 — 종료 코드로 판정한다.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $stdout = & $script:cliExe @Arguments 2> $cliStderr
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previous
    }
    $text = ($stdout | ForEach-Object { "$_" }) -join "`n"
    $stderr = Read-Shared $cliStderr
    Add-Content -LiteralPath $cliLog -Value "> kasaterm-cli $($Arguments -join ' ')`n[exit $code]`n$text`n$stderr" -Encoding utf8
    if ($code -ne 0) {
        throw "kasaterm-cli $($Arguments -join ' ') failed with exit code $code`n$text`n$stderr"
    }
    return $text
}

function Get-Screen {
    param([string]$Surface)
    # 좁은 칸에서는 한 줄이 다음 행으로 접힌다 — 행을 이어 붙여 본다.
    $text = [string]((Invoke-Cli -Arguments @("peek", $Surface)) | ConvertFrom-Json).result.text
    return ($text -replace "\r?\n", "")
}

function Get-SurfaceId {
    param([string]$Json, [string]$What)
    $result = ($Json | ConvertFrom-Json).result
    $surface = $result.surface
    if ($surface -is [string] -and $surface) {
        return $surface
    }
    foreach ($field in @("surface_id", "id")) {
        if ($surface -and $surface.$field) {
            return [string]$surface.$field
        }
    }
    throw "$What returned no surface id: $Json"
}

function Test-PaneRoundTrip {
    param([string]$Surface, [string]$Name)
    $null = Wait-Until -What "$Name prompt" -Seconds 30 -Condition {
        try {
            (Get-Screen $Surface) -match '\S'
        } catch {
            $false
        }
    }
    # 친 줄에는 base64 만 있고 nonce 는 없다. 그래서 화면의 `kasaterm-smoke-out:<nonce>` 는 셸이 실제로
    # 실행해 낸 출력이고(base64 에는 '-'·':' 가 없다), 파일 내용은 입력이 프로세스까지 닿았다는 증거다.
    # powershell.exe 는 칸의 셸이 pwsh·Windows PowerShell·cmd·Git Bash 어느 것이어도 같은 줄로 부를 수 있다.
    $nonce = "ks" + [guid]::NewGuid().ToString("N").Substring(0, 16)
    $expected = "kasaterm-smoke-out:$nonce"
    $marker = Join-Path $script:scratch "$nonce.txt"
    $quotedMarker = $marker.Replace("'", "''")
    $payload = "Set-Content -LiteralPath '$quotedMarker' -Value '$nonce' -Encoding ascii -NoNewline; " +
        "Write-Output ('kasaterm-smoke-out:' + '$nonce')"
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($payload))
    $line = "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand $encoded"

    $printed = $false
    foreach ($attempt in 1..2) {
        Invoke-Cli -Arguments @("tell", "--raw", $Surface, $line) | Out-Null
        Invoke-Cli -Arguments @("tell", "--key", $Surface, "enter") | Out-Null
        $printed = Wait-Until -What "$Name output" -Seconds 40 -Condition {
            try {
                (Get-Screen $Surface).Contains($expected)
            } catch {
                $false
            }
        }
        if ($printed) {
            break
        }
    }
    Assert-Check -Name "$Name ConPTY output" -Condition $printed `
        -Detail "peek $Surface shows '$expected' printed by the encoded command"
    $content = if (Test-Path -LiteralPath $marker) { (Read-Shared $marker).Trim() } else { "<missing>" }
    Assert-Check -Name "$Name ConPTY input" -Condition ($content -eq $nonce) `
        -Detail "marker file holds '$content' (expected $nonce)"
}

$appEnvironmentSaved = $null
$cliEnvironmentSaved = $null
$exitCode = $null
$failure = $null
$started = Get-Date
$script:scratch = $null
# CLI 는 UTF-8 JSON 을 낸다. 콘솔 코드 페이지로 읽으면 칸 글자가 깨져 JSON 해석이 흔들린다.
$consoleEncoding = [Console]::OutputEncoding
[Console]::OutputEncoding = [Text.Encoding]::UTF8

try {
    # 소켓 이름은 named pipe 이름이 된다(마지막 조각만 쓴다) — 실행마다 달라야 남의 파이프를 안 문다.
    $nonce = [guid]::NewGuid().ToString("N").Substring(0, 12)
    $tempBase = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
    $script:scratch = Join-Path $tempBase "ks-$nonce"
    New-Item -ItemType Directory -Path $script:scratch | Out-Null
    if ($PortableZip) {
        $zip = (Resolve-Path -LiteralPath $PortableZip).Path
        $extractRoot = Join-Path $script:scratch "app"
        Expand-Archive -LiteralPath $zip -DestinationPath $extractRoot
        $AppDirectory = Join-Path $extractRoot "kasaterm"
    }
    if (-not $AppDirectory) {
        $AppDirectory = Join-Path $repoRoot "target\release"
    }
    $appDir = (Resolve-Path -LiteralPath $AppDirectory).Path
    $appExe = Join-Path $appDir "kasaterm.exe"
    $script:cliExe = Join-Path $appDir "kasaterm-cli.exe"
    foreach ($binary in @($appExe, $script:cliExe)) {
        Assert-Check -Name "binary present" -Condition (Test-Path -LiteralPath $binary) -Detail $binary
    }

    $versionLine = Select-String -LiteralPath (Join-Path $repoRoot "Cargo.toml") `
        -Pattern '^version = "([0-9]+\.[0-9]+\.[0-9]+)"' | Select-Object -First 1
    $version = $versionLine.Matches[0].Groups[1].Value

    foreach ($dir in @("home", "appdata", "localappdata", "tmp", "students", "collab", "share")) {
        New-Item -ItemType Directory -Path (Join-Path $script:scratch $dir) | Out-Null
    }
    $socketPath = Join-Path $script:scratch "ks-$nonce.sock"
    $portFile = [IO.Path]::ChangeExtension($socketPath, ".mcp_port")
    $realHome = [Environment]::GetFolderPath("UserProfile")
    $realTemp = [IO.Path]::GetTempPath()
    $registryBefore = Get-RegistrySnapshot $userRegistry

    $appEnvironment = [ordered]@{
        HOME = Join-Path $script:scratch "home"
        USERPROFILE = Join-Path $script:scratch "home"
        APPDATA = Join-Path $script:scratch "appdata"
        LOCALAPPDATA = Join-Path $script:scratch "localappdata"
        TEMP = Join-Path $script:scratch "tmp"
        TMP = Join-Path $script:scratch "tmp"
        TMPDIR = Join-Path $script:scratch "tmp"
        KASATERM_SESSION_FILE = Join-Path $script:scratch "session.json"
        KASATERM_SETTINGS_FILE = Join-Path $script:scratch "settings.json"
        KASATERM_WINDOW_FILE = Join-Path $script:scratch "window.json"
        KASATERM_DEVICE_FILE = Join-Path $script:scratch "device.json"
        KASATERM_STUDENTS_DIR = Join-Path $script:scratch "students"
        KASATERM_COLLAB_ROOT = Join-Path $script:scratch "collab"
        KASATERM_SHARE_DIR = Join-Path $script:scratch "share"
        KASATERM_MOBILE_USERS = Join-Path $script:scratch "mobile-users.json"
        KASATERM_KASANET_KEY = Join-Path $script:scratch "kasanet.key"
        KASATERM_KASANET = "off"
        KASATERM_MACHINES = "[]"
        KASATERM_SOCKET_PATH = $socketPath
        CMUX_SOCKET_PATH = $socketPath
        KASATERM_AUTORESTORE = "fresh"
        KASATERM_AUTOQUIT_MS = "$($AutoQuitSeconds * 1000)"
        KASATERM_NO_FOCUS = "1"
        KASATERM_WINDOW_SIZE = "1280,800"
        KASATERM_AUTOCAPTURE_MS = "20000"
        KASATERM_AUTOCAPTURE_PATH = $capturePath
        KASATERM_SHELL = $Shell
        KASATERM_PANE_ID = $null
        KASATERM_TMUX_SHIM_DIR = $null
        KASATERM_ARONA_UI_DIR = $null
        KASATERM_LITE_ROOT = $null
        NO_COLOR = $null
    }
    $bundledUi = Join-Path $appDir "arona-ui\index.html"
    $repoUi = Join-Path $repoRoot "web\arona-ui\dist"
    if (-not (Test-Path -LiteralPath $bundledUi) -and (Test-Path -LiteralPath (Join-Path $repoUi "index.html"))) {
        $appEnvironment["KASATERM_ARONA_UI_DIR"] = $repoUi
    }

    Write-Host "== kasaterm smoke ($Label): $appExe =="
    Write-Host "   scratch:   $($script:scratch)"
    Write-Host "   artifacts: $artifacts"
    $appEnvironmentSaved = Set-ProcessEnvironment $appEnvironment
    try {
        $script:proc = Start-Process -FilePath $appExe -WorkingDirectory $script:scratch -PassThru `
            -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog
        $null = $script:proc.Handle
    } finally {
        Restore-ProcessEnvironment $appEnvironmentSaved
        $appEnvironmentSaved = $null
    }
    Add-Result -Name "launch" -Ok $true -Detail "pid $($script:proc.Id)"

    $cliEnvironmentSaved = Set-ProcessEnvironment ([ordered]@{
        KASATERM_SOCKET_PATH = $socketPath
        CMUX_SOCKET_PATH = $socketPath
        KASATERM_PANE_ID = $null
    })

    $ready = Wait-Until -What "socket and HTTP" -Seconds $ReadyTimeoutSeconds -Condition {
        (Test-Path -LiteralPath $portFile) -and ((Read-Shared $stderrLog) -match '\[agent-socket\] listening on')
    }
    Assert-Check -Name "boot" -Condition $ready -Detail "socket + HTTP port within ${ReadyTimeoutSeconds}s`n$(if (-not $ready) { Get-LogTail })"

    $log = Read-Shared $stderrLog
    $gpu = [regex]::Match($log, '\[gpu\] backend=(\S+) device="([^"]*)" type=(\S+)')
    Assert-Check -Name "GPU surface" -Condition $gpu.Success `
        -Detail $(if ($gpu.Success) { "$($gpu.Groups[1].Value) / $($gpu.Groups[2].Value) / $($gpu.Groups[3].Value)" } else { "no [gpu] line" })

    $port = (Read-Shared $portFile).Trim()
    $info = Invoke-RestMethod -Uri "http://127.0.0.1:$port/version" -TimeoutSec 15
    Assert-Check -Name "HTTP /version" -Condition ($info.ok -eq $true -and $info.version -eq $version) `
        -Detail "port $port, version $($info.version) (expected $version)"
    $page = Invoke-WebRequest -Uri "http://127.0.0.1:$port/term" -UseBasicParsing -TimeoutSec 15
    Assert-Check -Name "HTTP /term" -Condition ($page.StatusCode -eq 200) -Detail "status $($page.StatusCode)"

    # 소켓은 첫 칸보다 먼저 선다(칸 셸이 소켓 경로를 물려받아야 해서) — 칸이 등록될 때까지 다시 묻는다.
    $script:first = $null
    $found = Wait-Until -What "first pane" -Seconds 30 -Condition {
        try {
            $script:first = Get-SurfaceId -Json (Invoke-Cli -Arguments @("identify")) -What "identify"
            $true
        } catch {
            $false
        }
    }
    $first = $script:first
    Assert-Check -Name "socket identify" -Condition $found -Detail "first surface $first over $socketPath"
    Test-PaneRoundTrip -Surface $first -Name "first pane"

    $split = Invoke-Cli -Arguments @("split", "right")
    $second = Get-SurfaceId -Json $split -What "split"
    Assert-Check -Name "socket split" -Condition ($second -ne $first) -Detail "new surface $second"
    Test-PaneRoundTrip -Surface $second -Name "split pane"

    $captured = Wait-Until -What "window capture" -Seconds 40 -Condition {
        (Test-Path -LiteralPath $capturePath) -and (Get-Item -LiteralPath $capturePath).Length -gt 4096
    }
    $magic = if ($captured) { [IO.File]::ReadAllBytes($capturePath)[0..3] } else { @() }
    Assert-Check -Name "window capture" -Condition ($captured -and $magic[0] -eq 0x89 -and $magic[1] -eq 0x50) `
        -Detail $(if ($captured) { "$capturePath ($((Get-Item -LiteralPath $capturePath).Length) bytes)" } else { "no PNG" })

    Restore-ProcessEnvironment $cliEnvironmentSaved
    $cliEnvironmentSaved = $null

    $graceMs = ($AutoQuitSeconds + 45) * 1000 - [int]((Get-Date) - $started).TotalMilliseconds
    Update-Descendants
    $exited = $script:proc.WaitForExit([math]::Max($graceMs, 5000))
    if ($exited) {
        $exitCode = $script:proc.ExitCode
    }
    Assert-Check -Name "autoquit exit" -Condition ($exited -and $exitCode -eq 0) `
        -Detail $(if ($exited) { "exit code $exitCode" } else { "still running after autoquit deadline" })

    # 앱이 끝나면 칸 셸·콘솔 호스트·웹뷰도 따라 끝나야 한다. 유예 뒤에도 남으면 ConPTY·자식 정리 회귀다.
    $deadline = (Get-Date).AddSeconds($ChildGraceSeconds)
    do {
        $lingering = @(Get-LiveTracked)
        if ($lingering.Count -eq 0) {
            break
        }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    $lingeringText = ($lingering | ForEach-Object { "$($_.Name)#$($_.ProcessId) [$($_.CommandLine)]" }) -join "; "
    Assert-Check -Name "children exit with app" -Condition ($lingering.Count -eq 0) `
        -Detail $(if ($lingering.Count) { "still running ${ChildGraceSeconds}s after exit: $lingeringText" } else { "all $($script:tracked.Count) descendants gone" })

    $panicLog = Join-Path $appEnvironment["TEMP"] "kasaterm-panic.log"
    Assert-Check -Name "no panic" -Condition (-not (Test-Path -LiteralPath $panicLog)) `
        -Detail $(if (Test-Path -LiteralPath $panicLog) { Read-Shared $panicLog } else { "no panic log" })

    # 사람이 쓰는 kasaterm 이 같이 떠 있으면 그 앱이 자기 설정·레지스트리를 쓴다 — 그때는 판정하지 않는다.
    $others = @(Get-Process -Name kasaterm -ErrorAction SilentlyContinue)
    if ($others.Count -eq 0) {
        $leaks = @()
        $realConfig = Join-Path $realHome ".config\kasaterm"
        if (Test-Path -LiteralPath $realConfig) {
            $leaks += @(Get-ChildItem -LiteralPath $realConfig -Recurse -File -Force -ErrorAction SilentlyContinue |
                Where-Object { $_.LastWriteTime -ge $started } | ForEach-Object { $_.FullName })
        }
        $leaks += @(Get-ChildItem -LiteralPath $realTemp -Filter "kasaterm*" -Force -ErrorAction SilentlyContinue |
            Where-Object { $_.LastWriteTime -ge $started } | ForEach-Object { $_.FullName })
        Assert-Check -Name "user files untouched" -Condition ($leaks.Count -eq 0) `
            -Detail $(if ($leaks.Count) { $leaks -join "; " } else { "nothing under real ~/.config/kasaterm or %TEMP%\kasaterm*" })
        $registryAfter = Get-RegistrySnapshot $userRegistry
        Assert-Check -Name "user registry untouched" -Condition ($registryAfter -eq $registryBefore) `
            -Detail $(if ($registryAfter -eq $registryBefore) { "$userRegistry unchanged" } else { "before:`n$registryBefore`nafter:`n$registryAfter" })
    } else {
        Add-Result -Name "user state untouched" -Ok $true -Detail "not judged: another kasaterm is running"
    }
} catch {
    $failure = $_
    if (-not ($results | Where-Object { -not $_.ok })) {
        Add-Result -Name "smoke" -Ok $false -Detail "$($_.Exception.Message)"
    }
} finally {
    if ($appEnvironmentSaved) {
        Restore-ProcessEnvironment $appEnvironmentSaved
    }
    if ($cliEnvironmentSaved) {
        Restore-ProcessEnvironment $cliEnvironmentSaved
    }
    [Console]::OutputEncoding = $consoleEncoding
    Stop-Tracked
    # 이번 실행이 만든 폴더만 지운다 — 기록할 상태 파일은 먼저 결과 폴더로 옮긴다.
    if ($script:scratch -and (Test-Path -LiteralPath $script:scratch)) {
        $stateCopy = Join-Path $artifacts "state"
        New-Item -ItemType Directory -Path $stateCopy -Force | Out-Null
        Get-ChildItem -LiteralPath $script:scratch -File -Filter "*.json" -ErrorAction SilentlyContinue |
            Copy-Item -Destination $stateCopy -Force -ErrorAction SilentlyContinue
        Get-ChildItem -LiteralPath (Join-Path $script:scratch "tmp") -File -Filter "kasaterm*.log" -ErrorAction SilentlyContinue |
            Copy-Item -Destination $stateCopy -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $script:scratch -Recurse -Force -ErrorAction SilentlyContinue
    }

    $summary = [pscustomobject]@{
        label = $Label
        run = $runId
        ok = (-not $failure)
        exit_code = $exitCode
        started = $started.ToString("o")
        results = $results
    }
    $summary | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $artifacts "summary.json") -Encoding utf8
    if ($env:GITHUB_STEP_SUMMARY) {
        $rows = $results | ForEach-Object {
            $detail = ($_.detail -replace "`r?`n", " ") -replace '\|', '\|'
            if ($detail.Length -gt 300) { $detail = $detail.Substring(0, 300) + "…" }
            "| $(if ($_.ok) { 'PASS' } else { 'FAIL' }) | $($_.check) | $detail |"
        }
        $markdown = @("### Windows smoke: $Label ($runId)", "", "| | check | detail |", "|---|---|---|") + $rows + @("")
        Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Value ($markdown -join "`n") -Encoding utf8
    }
}

if ($failure) {
    Write-Host ""
    Write-Host "---- app stderr (tail) ----"
    Write-Host (Get-LogTail)
    throw $failure
}
Write-Host "== smoke ($Label) passed: $artifacts =="
