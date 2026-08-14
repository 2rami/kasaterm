[CmdletBinding()]
param(
    [string]$Repo = (Resolve-Path "$PSScriptRoot\..\.."),
    [string]$OutputDirectory,
    [string]$ExpectedVersion,
    [switch]$SkipBuild,
    [switch]$SkipUi
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

function Invoke-External {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$ArgumentList
    )

    & $FilePath @ArgumentList
    if ($LASTEXITCODE -ne 0) {
        throw "$FilePath failed with exit code $LASTEXITCODE"
    }
}

function Reset-Directory {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$AllowedRoot
    )

    $fullPath = [IO.Path]::GetFullPath($Path)
    $fullRoot = [IO.Path]::GetFullPath($AllowedRoot).TrimEnd('\') + '\'
    if (-not $fullPath.StartsWith($fullRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to reset a directory outside $fullRoot`: $fullPath"
    }
    if (Test-Path -LiteralPath $fullPath) {
        Remove-Item -LiteralPath $fullPath -Recurse -Force
    }
    New-Item -ItemType Directory -Path $fullPath -Force | Out-Null
    return $fullPath
}

function Get-VerifiedDownload {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][string]$Sha256
    )

    if (Test-Path -LiteralPath $Destination) {
        $actual = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash
        if ($actual -eq $Sha256) {
            return
        }
        Remove-Item -LiteralPath $Destination -Force
    }

    Write-Host "-- download: $Uri"
    Invoke-WebRequest -Uri $Uri -OutFile $Destination
    $actual = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash
    if ($actual -ne $Sha256) {
        Remove-Item -LiteralPath $Destination -Force
        throw "checksum mismatch for $Uri"
    }
}

function Assert-PackageFiles {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string[]]$Names
    )

    $files = Get-ChildItem -LiteralPath $Root -Recurse -File
    foreach ($name in $Names) {
        if (-not ($files | Where-Object { $_.Name -eq $name })) {
            throw "package verification failed: $name is missing"
        }
    }
}

function Assert-MsiManifest {
    param(
        [Parameter(Mandatory = $true)][string]$Manifest,
        [Parameter(Mandatory = $true)][string[]]$Names
    )

    [xml]$document = Get-Content -LiteralPath $Manifest -Encoding utf8
    $fileNodes = @($document.SelectNodes("//*[local-name()='File']"))
    foreach ($name in $Names) {
        $node = $fileNodes | Where-Object { $_.Name -eq $name } | Select-Object -First 1
        if (-not $node) {
            throw "MSI verification failed: $name is missing from the manifest"
        }
        if (-not (Test-Path -LiteralPath $node.Source)) {
            throw "MSI verification failed: payload for $name was not extracted"
        }
    }
}

$repoRoot = (Resolve-Path -LiteralPath $Repo).Path
$targetRoot = Join-Path $repoRoot "target"
$releaseRoot = Join-Path $targetRoot "release"
$workRoot = Join-Path $targetRoot "package-windows-x64"
$toolsRoot = Join-Path $targetRoot "package-tools"
$downloadsRoot = Join-Path $toolsRoot "downloads"
$distRoot = if ($OutputDirectory) {
    [IO.Path]::GetFullPath((Join-Path $repoRoot $OutputDirectory))
} else {
    Join-Path $repoRoot "dist"
}

Set-Location $repoRoot
New-Item -ItemType Directory -Path $downloadsRoot -Force | Out-Null
New-Item -ItemType Directory -Path $distRoot -Force | Out-Null

$versionLine = Select-String -LiteralPath (Join-Path $repoRoot "Cargo.toml") -Pattern '^version = "([0-9]+\.[0-9]+\.[0-9]+)"' | Select-Object -First 1
if (-not $versionLine) {
    throw "workspace version not found in Cargo.toml"
}
$version = $versionLine.Matches[0].Groups[1].Value
if ($ExpectedVersion) {
    $normalizedExpected = $ExpectedVersion.TrimStart('v')
    if ($normalizedExpected -ne $version) {
        throw "Cargo.toml version ($version) does not match expected version ($ExpectedVersion)"
    }
}

if ($env:PROCESSOR_ARCHITECTURE -ne "AMD64") {
    throw "this package currently supports x64 Windows only; host architecture is $env:PROCESSOR_ARCHITECTURE"
}

$uiRoot = Join-Path $repoRoot "web\arona-ui"
$uiDist = Join-Path $uiRoot "dist"
if (-not $SkipUi) {
    Write-Host "-- build arona-ui"
    Push-Location $uiRoot
    try {
        Invoke-External -FilePath "npm.cmd" -ArgumentList @("ci")
        Invoke-External -FilePath "npm.cmd" -ArgumentList @("run", "build")
    } finally {
        Pop-Location
    }
}
if (-not (Test-Path -LiteralPath (Join-Path $uiDist "index.html"))) {
    throw "arona-ui build output is missing: $uiDist"
}

if (-not $SkipBuild) {
    Write-Host "-- build release binaries"
    Invoke-External -FilePath "cargo.exe" -ArgumentList @("build", "--release", "-p", "kasaterm")
    Invoke-External -FilePath "cargo.exe" -ArgumentList @("build", "--release", "-p", "kasa-socket", "--bin", "kasaterm-cli")
}

$appExe = Join-Path $releaseRoot "kasaterm.exe"
$cliExe = Join-Path $releaseRoot "kasaterm-cli.exe"
foreach ($artifact in @($appExe, $cliExe)) {
    if (-not (Test-Path -LiteralPath $artifact)) {
        throw "build artifact is missing: $artifact"
    }
}

$winSparkleVersion = "0.9.3"
$winSparkleZip = Join-Path $downloadsRoot "WinSparkle-$winSparkleVersion.zip"
Get-VerifiedDownload `
    -Uri "https://github.com/vslavik/winsparkle/releases/download/v$winSparkleVersion/WinSparkle-$winSparkleVersion.zip" `
    -Destination $winSparkleZip `
    -Sha256 "745985F41D2AB26B2D5A1CF87D76E4ED851039DB19038E50610EB25EA0B73772"

$winSparkleRoot = Reset-Directory -Path (Join-Path $workRoot "winsparkle") -AllowedRoot $targetRoot
Expand-Archive -LiteralPath $winSparkleZip -DestinationPath $winSparkleRoot -Force
$winSparkleDll = Get-ChildItem -LiteralPath $winSparkleRoot -Recurse -Filter "WinSparkle.dll" -File |
    Where-Object { $_.FullName -match '[\\/]x64[\\/]Release[\\/]' } |
    Select-Object -First 1
if (-not $winSparkleDll) {
    throw "x64 WinSparkle.dll was not found in $winSparkleZip"
}
Copy-Item -LiteralPath $winSparkleDll.FullName -Destination (Join-Path $releaseRoot "WinSparkle.dll") -Force

$wixVersion = "3.14.1"
$wixZip = Join-Path $downloadsRoot "wix314-binaries.zip"
Get-VerifiedDownload `
    -Uri "https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314-binaries.zip" `
    -Destination $wixZip `
    -Sha256 "6AC824E1642D6F7277D0ED7EA09411A508F6116BA6FAE0AA5F2C7DAA2FF43D31"

$wixRoot = Join-Path $toolsRoot "wix-$wixVersion"
if (-not (Test-Path -LiteralPath (Join-Path $wixRoot "candle.exe"))) {
    $wixRoot = Reset-Directory -Path $wixRoot -AllowedRoot $targetRoot
    Expand-Archive -LiteralPath $wixZip -DestinationPath $wixRoot -Force
}
$heatExe = Join-Path $wixRoot "heat.exe"
$candleExe = Join-Path $wixRoot "candle.exe"
$lightExe = Join-Path $wixRoot "light.exe"
$darkExe = Join-Path $wixRoot "dark.exe"
$wixUiExtension = Join-Path $wixRoot "WixUIExtension.dll"
foreach ($tool in @($heatExe, $candleExe, $lightExe, $darkExe, $wixUiExtension)) {
    if (-not (Test-Path -LiteralPath $tool)) {
        throw "WiX tool is missing: $tool"
    }
}

$stageRoot = Reset-Directory -Path (Join-Path $workRoot "stage") -AllowedRoot $targetRoot
$collabStage = Join-Path $stageRoot "collab-hooks"
New-Item -ItemType Directory -Path $collabStage -Force | Out-Null
Get-ChildItem -LiteralPath (Join-Path $repoRoot "app\kasaterm\collab-hooks") -File |
    Where-Object { $_.Extension -ne ".md" } |
    Copy-Item -Destination $collabStage -Force

$wixBuildRoot = Reset-Directory -Path (Join-Path $workRoot "wix") -AllowedRoot $targetRoot
$aronaWxs = Join-Path $wixBuildRoot "aronaui.wxs"
$collabWxs = Join-Path $wixBuildRoot "collabhooks.wxs"
Invoke-External -FilePath $heatExe -ArgumentList @(
    "dir", $uiDist, "-cg", "AronaUiGroup", "-dr", "AronaUiDir",
    "-var", "var.AronaUiDir", "-srd", "-sfrag", "-gg", "-g1", "-sreg",
    "-scom", "-nologo", "-out", $aronaWxs
)
Invoke-External -FilePath $heatExe -ArgumentList @(
    "dir", $collabStage, "-cg", "CollabHooksGroup", "-dr", "CollabHooksDir",
    "-var", "var.CollabHooksDir", "-srd", "-sfrag", "-gg", "-g1", "-sreg",
    "-scom", "-nologo", "-out", $collabWxs
)

$mainWxs = Join-Path $repoRoot "app\kasaterm\wix\main.wxs"
$mainObj = Join-Path $wixBuildRoot "main.wixobj"
$aronaObj = Join-Path $wixBuildRoot "aronaui.wixobj"
$collabObj = Join-Path $wixBuildRoot "collabhooks.wixobj"
Invoke-External -FilePath $candleExe -ArgumentList @(
    "-nologo", "-arch", "x64", "-dVersion=$version", "-dCargoTargetBinDir=$releaseRoot",
    "-out", $mainObj, $mainWxs
)
Invoke-External -FilePath $candleExe -ArgumentList @(
    "-nologo", "-arch", "x64", "-dAronaUiDir=$uiDist", "-out", $aronaObj, $aronaWxs
)
Invoke-External -FilePath $candleExe -ArgumentList @(
    "-nologo", "-arch", "x64", "-dCollabHooksDir=$collabStage", "-out", $collabObj, $collabWxs
)

$baseName = "kasaterm-v$version-windows-x86_64"
$msiPath = Join-Path $distRoot "$baseName.msi"
$wixPdbPath = [IO.Path]::ChangeExtension($msiPath, ".wixpdb")
if (Test-Path -LiteralPath $wixPdbPath) {
    Remove-Item -LiteralPath $wixPdbPath -Force
}
Invoke-External -FilePath $lightExe -ArgumentList @(
    "-nologo", "-spdb", "-ext", $wixUiExtension, "-cultures:en-us", "-out", $msiPath,
    $mainObj, $aronaObj, $collabObj
)

$portableStage = Reset-Directory -Path (Join-Path $workRoot "portable\kasaterm") -AllowedRoot $targetRoot
Copy-Item -LiteralPath $appExe, $cliExe, (Join-Path $releaseRoot "WinSparkle.dll") -Destination $portableStage -Force
Copy-Item -LiteralPath (Join-Path $repoRoot "app\kasaterm\wix\License.rtf") -Destination $portableStage -Force
Copy-Item -LiteralPath $uiDist -Destination (Join-Path $portableStage "arona-ui") -Recurse -Force
Copy-Item -LiteralPath $collabStage -Destination (Join-Path $portableStage "collab-hooks") -Recurse -Force

$portableZip = Join-Path $distRoot "$baseName-portable.zip"
if (Test-Path -LiteralPath $portableZip) {
    Remove-Item -LiteralPath $portableZip -Force
}
Compress-Archive -LiteralPath $portableStage -DestinationPath $portableZip -CompressionLevel Optimal

$verifyRoot = Reset-Directory -Path (Join-Path $workRoot "verify") -AllowedRoot $targetRoot
$verifyFiles = Join-Path $verifyRoot "files"
$verifyWxs = Join-Path $verifyRoot "package.wxs"
Invoke-External -FilePath $darkExe -ArgumentList @(
    "-nologo", "-x", $verifyFiles, "-o", $verifyWxs, $msiPath
)
$requiredFiles = @(
    "kasaterm.exe", "kasaterm-cli.exe", "WinSparkle.dll", "index.html",
    "characters.json", "kasacollab.py", "statusline.py"
)
Assert-MsiManifest -Manifest $verifyWxs -Names $requiredFiles
Assert-PackageFiles -Root $portableStage -Names $requiredFiles

foreach ($artifact in @($msiPath, $portableZip)) {
    $hash = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "$artifact.sha256" -Value "$hash  $([IO.Path]::GetFileName($artifact))" -Encoding ascii
}

Write-Host ""
Write-Host "Windows packages verified:"
Write-Host "  $msiPath"
Write-Host "  $portableZip"
