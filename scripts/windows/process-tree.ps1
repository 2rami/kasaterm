function Get-SmokeProcessKey {
    param([object]$Process)
    if ($null -eq $Process -or $null -eq $Process.CreationDate) {
        return $null
    }
    return "$($Process.ProcessId):$(([datetime]$Process.CreationDate).ToUniversalTime().Ticks)"
}

function Get-SmokeDescendants {
    param([object[]]$Snapshot, [object]$Root, [object[]]$Known = @())
    $byId = @{}
    foreach ($process in $Snapshot) {
        if (Get-SmokeProcessKey $process) { $byId[[string]$process.ProcessId] = $process }
    }
    $rootKey = Get-SmokeProcessKey $Root
    $queue = New-Object 'System.Collections.Generic.Queue[object]'
    $seen = New-Object 'System.Collections.Generic.HashSet[string]'
    foreach ($seed in (@($Root) + @($Known))) {
        $key = Get-SmokeProcessKey $seed
        if (-not $key) { continue }
        $current = $byId[[string]$seed.ProcessId]
        if ((Get-SmokeProcessKey $current) -eq $key) { $queue.Enqueue($current) }
    }
    while ($queue.Count -gt 0) {
        $parent = $queue.Dequeue()
        $key = Get-SmokeProcessKey $parent
        if (-not $seen.Add($key)) { continue }
        if ($key -ne $rootKey) { $parent }
        foreach ($child in $Snapshot) {
            # ParentProcessId can point at a reused PID whose new process is younger than the child.
            if ($child.ParentProcessId -eq $parent.ProcessId -and (Get-SmokeProcessKey $child) -and
                ([datetime]$child.CreationDate).ToUniversalTime() -ge ([datetime]$parent.CreationDate).ToUniversalTime()) {
                $queue.Enqueue($child)
            }
        }
    }
}
