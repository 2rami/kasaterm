$ErrorActionPreference = "Stop"
. "$PSScriptRoot/process-tree.ps1"

function Process-Row {
    param([int]$Id, [int]$Parent, [int]$Created, [string]$Name = "fixture.exe")
    [pscustomobject]@{
        ProcessId = $Id
        ParentProcessId = $Parent
        CreationDate = ([datetime]"2026-01-01T00:00:00Z").AddSeconds($Created)
        Name = $Name
    }
}

function Assert-Ids {
    param([string]$Case, [object[]]$Rows, [int[]]$Expected)
    $actual = @($Rows | ForEach-Object { [int]$_.ProcessId } | Sort-Object) -join ','
    $wanted = @($Expected | Sort-Object) -join ','
    if ($actual -ne $wanted) { throw "${Case}: expected [$wanted], got [$actual]" }
    Write-Host "PASS $Case"
}

$root = Process-Row 10 1 20 "kasaterm.exe"
$child = Process-Row 11 10 21
$grandchild = Process-Row 12 11 22
$older = Process-Row 13 10 10 "vctip.exe"
$olderBranch = Process-Row 14 13 11
$snapshot = @($grandchild, $olderBranch, $child, $older, $root)
Assert-Ids -Case "older process with a reused parent PID is not an app child" `
    -Rows @(Get-SmokeDescendants -Snapshot $snapshot -Root $root) -Expected @(11, 12)

$youngParent = Process-Row 20 10 30
$olderThanParent = Process-Row 21 20 25
$realChild = Process-Row 22 20 31
Assert-Ids -Case "creation time is checked at every generation" `
    -Rows @(Get-SmokeDescendants -Snapshot @($root, $youngParent, $olderThanParent, $realChild) -Root $root) -Expected @(20, 22)

$newChild = Process-Row 30 11 40
Assert-Ids -Case "a known child can reveal descendants after the root exits" `
    -Rows @(Get-SmokeDescendants -Snapshot @($child, $newChild) -Root $root -Known @($child)) -Expected @(11, 30)

$reusedChild = Process-Row 11 999 50
Assert-Ids -Case "a reused known PID cannot seed another tree" `
    -Rows @(Get-SmokeDescendants -Snapshot @($reusedChild, (Process-Row 31 11 51)) -Root $root -Known @($child)) -Expected @()

$reusedRoot = Process-Row 10 999 50
Assert-Ids -Case "a reused root PID is rejected" `
    -Rows @(Get-SmokeDescendants -Snapshot @($reusedRoot, (Process-Row 32 10 51)) -Root $root) -Expected @()

$missingTime = Process-Row 40 10 30
$missingTime.CreationDate = $null
Assert-Ids -Case "unknown creation time is not proof of ownership" `
    -Rows @(Get-SmokeDescendants -Snapshot @($root, $missingTime) -Root $root) -Expected @()

$realTelemetry = Process-Row 50 10 22 "vctip.exe"
Assert-Ids -Case "names never exempt real children from cleanup checks" `
    -Rows @(Get-SmokeDescendants -Snapshot @($root, $realTelemetry) -Root $root) -Expected @(50)

Assert-Ids -Case "duplicate seeds do not duplicate results" `
    -Rows @(Get-SmokeDescendants -Snapshot @($root, $child, $grandchild) -Root $root -Known @($child, $grandchild)) -Expected @(11, 12)
