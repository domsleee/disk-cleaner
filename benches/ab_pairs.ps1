<#
.SYNOPSIS
  Randomized paired A/B runner for warm-cache scan speed, with a confidence
  interval to assess whether the measured difference is conclusive.

.DESCRIPTION
  Each pair runs both binaries back to back, with the order within the pair
  randomized (half AB, half BA, shuffled from a recorded seed). Fixed A,B,A,B
  leaves B permanently second, inheriting A's cache warming -- a bias that
  looks exactly like a real effect.

  Requires binaries built from a scan_only.rs that prints the BENCH line.
  Scan time covers the complete scan_directory call, including dedup-set
  cleanup and sorting. Only teardown of the returned tree is timed separately.

  Reports the geometric mean of per-pair log ratios with a paired bootstrap CI.
  Median and min are diagnostics only.

.EXAMPLE
  .\benches\ab_pairs.ps1 -ExeA .\scan_a.exe -ExeB .\scan_b.exe -ScanPath C:\Users\me\projects
#>
param(
    [Parameter(Mandatory = $true)][string]$ExeA,
    [Parameter(Mandatory = $true)][string]$ExeB,
    [string]$LabelA = 'base',
    [string]$LabelB = 'mimalloc',
    [Parameter(Mandatory = $true)][string]$ScanPath,
    [ValidateRange(20, 2147483647)]
    [ValidateScript({ $_ % 2 -eq 0 })][int]$Pairs = 20,
    [ValidateRange(0, 2147483647)][int]$Warmup = 3,
    [int]$Seed = 20260912,
    [ValidateRange(100, 2147483647)][int]$Boot = 10000,
    [string]$CsvPath = ''
)

$ErrorActionPreference = 'Stop'

# PowerShell's location can differ from the process CWD after Set-Location.
# Resolve every input before starting a native process, including relative
# scan paths; otherwise another checkout's binary or tree could be measured.
$runDirectory = (Get-Location).ProviderPath
$ExeA = Convert-Path -LiteralPath $ExeA
$ExeB = Convert-Path -LiteralPath $ExeB
$ScanPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ScanPath)
if ($CsvPath -ne '') {
    $CsvPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($CsvPath)
    # Clear previous results even if this run fails before completing a pair.
    Set-Content -LiteralPath $CsvPath -Value '' -NoNewline
}

function Invoke-Scan {
    param([string]$Exe, [string]$Arg)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.WorkingDirectory = $runDirectory
    if ($psi.PSObject.Properties['ArgumentList']) {
        $psi.ArgumentList.Add($Arg)
    } else {
        # Windows PowerShell 5.1: escape quotes and double trailing backslashes
        # before enclosing the argument in quotes (including drive roots).
        $escaped = $Arg -replace '(\\*)"', '$1$1\"'
        $escaped = $escaped -replace '(\\+)$', '$1$1'
        $psi.Arguments = '"' + $escaped + '"'
    }
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $p = [System.Diagnostics.Process]::Start($psi)
    try {
        # Drain both pipes concurrently so a full stderr pipe cannot hang a run.
        $outTask = $p.StandardOutput.ReadToEndAsync()
        $errTask = $p.StandardError.ReadToEndAsync()
        $p.WaitForExit()
        $sw.Stop()
        $out = $outTask.GetAwaiter().GetResult()
        $err = $errTask.GetAwaiter().GetResult()
        if ($p.ExitCode -ne 0) {
            throw "Scan failed: $Exe (exit $($p.ExitCode)): $err"
        }
    } finally {
        $p.Dispose()
    }

    $m = [regex]::Match($out, '(?m)^BENCH scan_ms=([0-9]+(?:\.[0-9]+)?) drop_ms=([0-9]+(?:\.[0-9]+)?) files=([0-9]+) bytes=([0-9]+)(?: scanner=(\w+))?\r?$')
    if (-not $m.Success) {
        throw "Failed to parse BENCH line from ${Exe}: $out $err"
    }
    $scanMs = [double]$m.Groups[1].Value
    $dropMs = [double]$m.Groups[2].Value
    $files = [uint64]$m.Groups[3].Value
    $bytes = [uint64]$m.Groups[4].Value
    if ($scanMs -le 0 -or [double]::IsInfinity($scanMs) -or [double]::IsInfinity($dropMs)) {
        throw "Invalid BENCH timing from ${Exe}: scan_ms=$scanMs drop_ms=$dropMs"
    }
    $scanner = if ($m.Groups[5].Success) { $m.Groups[5].Value } else { 'unknown' }
    [pscustomobject]@{
        ScanMs = $scanMs; DropMs = $dropMs; Files = $files; Bytes = $bytes; Scanner = $scanner
        ProcMs = $sw.Elapsed.TotalMilliseconds
    }
}

function Get-Median {
    param([double[]]$v)
    if ($v.Count -eq 0) { return [double]::NaN }
    $s = $v | Sort-Object; $n = $s.Count
    if ($n % 2 -eq 1) { return $s[[int](($n - 1) / 2)] }
    return ($s[$n / 2 - 1] + $s[$n / 2]) / 2
}

$rand = New-Object System.Random($Seed)

Write-Host "Scan target : $ScanPath"
Write-Host "Pairs       : $Pairs (randomized AB/BA, seed $Seed)"
Write-Host "Warmup      : $Warmup per arm"
Write-Host ''

# Warm the metadata cache for both arms before any measurement.
for ($i = 0; $i -lt $Warmup; $i++) {
    $null = Invoke-Scan -Exe $ExeA -Arg $ScanPath
    $null = Invoke-Scan -Exe $ExeB -Arg $ScanPath
}

# Half the pairs run A first, half B first, then shuffle the schedule.
$order = @()
for ($i = 0; $i -lt $Pairs; $i++) { $order += ($i -lt [math]::Floor($Pairs / 2)) }
$order = $order | Sort-Object { $rand.Next() }

$rows = @()
for ($i = 0; $i -lt $Pairs; $i++) {
    $aFirst = $order[$i]
    if ($aFirst) {
        $ra = Invoke-Scan -Exe $ExeA -Arg $ScanPath
        $rb = Invoke-Scan -Exe $ExeB -Arg $ScanPath
    } else {
        $rb = Invoke-Scan -Exe $ExeB -Arg $ScanPath
        $ra = Invoke-Scan -Exe $ExeA -Arg $ScanPath
    }

    if ($ra.Files -ne $rb.Files -or $ra.Bytes -ne $rb.Bytes) {
        throw "Pair $($i+1): scan result mismatch (A: $($ra.Files) files, $($ra.Bytes) bytes; B: $($rb.Files) files, $($rb.Bytes) bytes)"
    }

    $rows += [pscustomobject]@{
        Pair = $i + 1
        Order = if ($aFirst) { 'AB' } else { 'BA' }
        AScan = $ra.ScanMs; BScan = $rb.ScanMs
        ADrop = $ra.DropMs; BDrop = $rb.DropMs
        AProc = $ra.ProcMs; BProc = $rb.ProcMs
        Files = $ra.Files; Bytes = $ra.Bytes
        LogRatio = [math]::Log($rb.ScanMs / $ra.ScanMs)
    }
    if ($CsvPath -ne '') {
        $rows[-1] | Export-Csv -NoTypeInformation -LiteralPath $CsvPath -Append:($i -gt 0)
    }
    Write-Host ("  pair {0,2} [{1}]  {2} {3,8:N1} ms  {4} {5,8:N1} ms   drop {6,6:N1}/{7,6:N1}" -f `
        ($i + 1), $rows[-1].Order, $LabelA, $ra.ScanMs, $LabelB, $rb.ScanMs, $ra.DropMs, $rb.DropMs)
}

$d = @($rows | ForEach-Object { $_.LogRatio })
$n = $d.Count
$mean = ($d | Measure-Object -Average).Average
$ratio = [math]::Exp($mean)

# Paired bootstrap: resample whole pairs, so within-pair correlation is kept.
$boots = New-Object double[] $Boot
for ($b = 0; $b -lt $Boot; $b++) {
    $s = 0.0
    for ($k = 0; $k -lt $n; $k++) { $s += $d[$rand.Next($n)] }
    $boots[$b] = [math]::Exp($s / $n)
}
$sorted = $boots | Sort-Object
$lo = $sorted[[int][math]::Floor(0.025 * $Boot)]
$hi = $sorted[[int][math]::Floor(0.975 * $Boot)]

$aScan = @($rows | ForEach-Object { $_.AScan })
$bScan = @($rows | ForEach-Object { $_.BScan })
$aDrop = @($rows | ForEach-Object { $_.ADrop })
$bDrop = @($rows | ForEach-Object { $_.BDrop })
$aProc = @($rows | ForEach-Object { $_.AProc })
$bProc = @($rows | ForEach-Object { $_.BProc })

Write-Host ''
Write-Host '=== Scan time (the number that matters) ==='
Write-Host ("  {0,-10} median {1,8:N1} ms   min {2,8:N1} ms" -f $LabelA, (Get-Median $aScan), ($aScan | Measure-Object -Minimum).Minimum)
Write-Host ("  {0,-10} median {1,8:N1} ms   min {2,8:N1} ms" -f $LabelB, (Get-Median $bScan), ($bScan | Measure-Object -Minimum).Minimum)
Write-Host ''
Write-Host ("  geometric mean ratio {0}/{1} : {2:N4}" -f $LabelB, $LabelA, $ratio)
Write-Host ("  scan time change            : {0:N2}%  (95% CI {1:N2}% .. {2:N2}%)" -f (100 * ($ratio - 1)), (100 * ($lo - 1)), (100 * ($hi - 1)))
if ($lo -le 1 -and $hi -ge 1) {
    Write-Host '  VERDICT: INCONCLUSIVE - CI includes zero; this run did not resolve a scan-speed difference.'
} elseif ($hi -lt 1) {
    Write-Host ("  VERDICT: {0} is significantly FASTER on scan." -f $LabelB)
} else {
    Write-Host ("  VERDICT: {0} is significantly SLOWER on scan." -f $LabelB)
}

Write-Host ''
Write-Host '=== Teardown (excluded from scan, reported for completeness) ==='
Write-Host ("  {0,-10} median {1,8:N1} ms" -f $LabelA, (Get-Median $aDrop))
Write-Host ("  {0,-10} median {1,8:N1} ms" -f $LabelB, (Get-Median $bDrop))
Write-Host ''
Write-Host '=== Whole process (what the old harness measured) ==='
Write-Host ("  {0,-10} median {1,8:N1} ms" -f $LabelA, (Get-Median $aProc))
Write-Host ("  {0,-10} median {1,8:N1} ms" -f $LabelB, (Get-Median $bProc))
