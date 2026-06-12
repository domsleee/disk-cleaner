#Requires -Version 5.1
<#
.SYNOPSIS
  Cold-cache scan benchmark. Scans a VHDX-backed NTFS volume that is
  dismounted and remounted before every run, so each scan starts with a
  cold NTFS metadata cache. Requires an elevated (Administrator) shell.

.DESCRIPTION
  Hot-cache benchmarks (stat_bench, scan_bench) understate I/O-bound
  improvements because Windows caches NTFS metadata aggressively. This
  harness creates a reusable VHDX fixture and cycles its mount state
  between runs to invalidate the volume's caches without rebooting.

  What "cold" means here:
    - NTFS metadata/MFT cache for the bench volume: cold, guaranteed by
      the dismount/remount cycle.
    - Host page cache of the .vhdx backing file: warm, unless
      -PurgeStandby is passed (purges the system standby list, like
      RAMMap's "Empty Standby List").
  Anything colder than that needs the VHDX on a separate physical disk or
  a reboot. NTFS-cold is deterministic and covers most of the first-scan
  cost (per-directory syscalls + MFT record parsing).

  The VHDX is created and populated once, then reused across invocations
  (and across builds, for A/B comparisons). Use -Rebuild to repopulate,
  -Cleanup to delete it.

  Antivirus note: real-time scanning of a freshly mounted volume adds
  noise. If stddev is high, try excluding the VHDX path in Defender.

.EXAMPLE
  # First run: create + populate synthetic fixture, 5 cold runs
  .\benches\coldcache.ps1

.EXAMPLE
  # Fixture copied from a real directory, 10 runs, plus a hot re-scan
  .\benches\coldcache.ps1 -SourcePath C:\Users\me\projects -Runs 10 -AlsoHot

.EXAMPLE
  # A/B two refs against the same fixture
  git checkout main
  cargo build --release --features internal-tools --bin scan_only
  Copy-Item target\release\scan_only.exe $env:TEMP\scan_a.exe
  git checkout my-branch
  .\benches\coldcache.ps1 -Exe $env:TEMP\scan_a.exe -NoBuild
  .\benches\coldcache.ps1
#>
[CmdletBinding()]
param(
    # Number of cold (dismount/remount) runs.
    [int]$Runs = 5,

    # Location of the reusable VHDX fixture.
    [string]$VhdxPath,

    # Max size of the expandable VHDX, in MB.
    [int]$VhdxSizeMB = 8192,

    # Populate the fixture by copying this directory (robocopy /E).
    # Default: generate a synthetic tree instead.
    [string]$SourcePath,

    # Synthetic fixture shape: Dirs x Subdirs leaf directories, each with
    # Files files of 0-16KB. Defaults: 40 x 25 x 50 = 50,000 files.
    [int]$FixtureDirs = 40,
    [int]$FixtureSubdirs = 25,
    [int]$FixtureFiles = 50,

    # Path to a prebuilt scan_only.exe. Default: target\release\scan_only.exe.
    [string]$Exe,

    # Skip the cargo build step (use with -Exe or a previous build).
    [switch]$NoBuild,

    # Delete and recreate the VHDX fixture.
    [switch]$Rebuild,

    # Delete the VHDX fixture and exit.
    [switch]$Cleanup,

    # Purge the OS standby list before each cold run so reads of the
    # .vhdx backing file also miss the host page cache.
    [switch]$PurgeStandby,

    # After the last cold run, re-scan the still-mounted volume to report
    # a hot-cache number for comparison.
    [switch]$AlsoHot
)

$ErrorActionPreference = 'Stop'
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (-not $VhdxPath) { $VhdxPath = Join-Path $repoRoot 'target\coldcache\coldbench.vhdx' }
$VhdxPath = [IO.Path]::GetFullPath($VhdxPath)

function Assert-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($id)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'This script must run elevated (mounting VHDs requires Administrator).'
    }
}

function Test-BenchAttached {
    try { return (Get-DiskImage -ImagePath $VhdxPath -ErrorAction Stop).Attached }
    catch { return $false }
}

function Dismount-Bench {
    if (Test-BenchAttached) { Dismount-DiskImage -ImagePath $VhdxPath | Out-Null }
}

# Mount the VHDX (no-op if already attached, e.g. right after diskpart
# creation) and return the root path of its NTFS volume (e.g. "X:\").
function Mount-Bench {
    if (-not (Test-BenchAttached)) { Mount-DiskImage -ImagePath $VhdxPath | Out-Null }
    for ($i = 0; $i -lt 40; $i++) {
        $part = Get-DiskImage -ImagePath $VhdxPath | Get-Disk -ErrorAction SilentlyContinue |
            Get-Partition -ErrorAction SilentlyContinue | Where-Object DriveLetter
        if ($part) { return "$($part.DriveLetter):\" }
        # Letter not restored from a previous mount yet (or never assigned).
        $bare = Get-DiskImage -ImagePath $VhdxPath | Get-Disk -ErrorAction SilentlyContinue |
            Get-Partition -ErrorAction SilentlyContinue |
            Where-Object { $_.Type -ne 'Reserved' } | Select-Object -First 1
        if ($bare -and $i -ge 4) {
            $bare | Add-PartitionAccessPath -AssignDriveLetter -ErrorAction SilentlyContinue
        }
        Start-Sleep -Milliseconds 250
    }
    throw "Mounted $VhdxPath but no volume with a drive letter appeared."
}

function New-BenchVhdx {
    $dir = Split-Path $VhdxPath -Parent
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force $dir | Out-Null }

    Write-Host "Creating VHDX ($VhdxSizeMB MB expandable) at $VhdxPath ..."
    $dpScript = @"
create vdisk file="$VhdxPath" maximum=$VhdxSizeMB type=expandable
select vdisk file="$VhdxPath"
attach vdisk
create partition primary
format fs=ntfs quick label=COLDBENCH
assign
"@
    $dpFile = Join-Path $env:TEMP "coldcache-diskpart-$PID.txt"
    Set-Content -Path $dpFile -Value $dpScript -Encoding ascii
    try {
        $out = diskpart /s $dpFile
        if ($LASTEXITCODE -ne 0) {
            throw "diskpart failed (exit $LASTEXITCODE):`n$($out -join "`n")"
        }
    } finally {
        Remove-Item $dpFile -ErrorAction SilentlyContinue
    }
}

function Initialize-Fixture([string]$root) {
    if ($SourcePath) {
        if (-not (Test-Path $SourcePath -PathType Container)) {
            throw "SourcePath is not a directory: $SourcePath"
        }
        Write-Host "Populating fixture from $SourcePath (robocopy) ..."
        robocopy $SourcePath (Join-Path $root 'fixture') /E /MT:16 /R:0 /W:0 /NFL /NDL /NJH /NJS /NP | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "robocopy failed (exit $LASTEXITCODE)" }
        # robocopy exit codes 0-7 are success variants; don't leak them.
        $global:LASTEXITCODE = 0
    } else {
        $total = $FixtureDirs * $FixtureSubdirs * $FixtureFiles
        Write-Host "Generating synthetic fixture: $FixtureDirs x $FixtureSubdirs dirs, $total files ..."
        $rng = New-Object System.Random(42)
        $buf = New-Object byte[] 16384
        $rng.NextBytes($buf)
        $written = 0
        for ($d = 0; $d -lt $FixtureDirs; $d++) {
            for ($s = 0; $s -lt $FixtureSubdirs; $s++) {
                $leaf = Join-Path $root ("fixture\dir{0:d3}\sub{1:d3}" -f $d, $s)
                [IO.Directory]::CreateDirectory($leaf) | Out-Null
                for ($f = 0; $f -lt $FixtureFiles; $f++) {
                    $size = $rng.Next(0, $buf.Length + 1)
                    $fs = [IO.File]::Create((Join-Path $leaf ("file{0:d4}.bin" -f $f)))
                    $fs.Write($buf, 0, $size)
                    $fs.Close()
                    $written++
                }
            }
            if ($written % 10000 -lt $FixtureSubdirs * $FixtureFiles) {
                Write-Host "  $written / $total files"
            }
        }
        Write-Host "  $written files written."
    }
}

# Purge the system standby list (what RAMMap's "Empty Standby List" does)
# so the .vhdx backing file is evicted from the host page cache too.
function Invoke-StandbyPurge {
    if (-not ('ColdCache.Native' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace ColdCache {
  public static class Native {
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool OpenProcessToken(IntPtr proc, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool LookupPrivilegeValue(string sys, string name, out long luid);
    // Pack = 4: the Win32 struct has LUID at offset 4 (no 8-byte alignment).
    [StructLayout(LayoutKind.Sequential, Pack = 4)]
    struct TOKEN_PRIVILEGES { public uint Count; public long Luid; public uint Attr; }
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool AdjustTokenPrivileges(IntPtr token, bool disableAll,
        ref TOKEN_PRIVILEGES newState, int len, IntPtr prev, IntPtr retLen);
    [DllImport("ntdll.dll")]
    static extern int NtSetSystemInformation(int infoClass, ref int info, int len);

    public static void PurgeStandbyList() {
      IntPtr token;
      if (!OpenProcessToken(System.Diagnostics.Process.GetCurrentProcess().Handle,
          0x20 | 0x8 /* ADJUST_PRIVILEGES | QUERY */, out token))
        throw new InvalidOperationException("OpenProcessToken failed");
      TOKEN_PRIVILEGES tp = new TOKEN_PRIVILEGES();
      tp.Count = 1;
      tp.Attr = 2; // SE_PRIVILEGE_ENABLED
      if (!LookupPrivilegeValue(null, "SeProfileSingleProcessPrivilege", out tp.Luid))
        throw new InvalidOperationException("LookupPrivilegeValue failed");
      if (!AdjustTokenPrivileges(token, false, ref tp, 0, IntPtr.Zero, IntPtr.Zero)
          || Marshal.GetLastWin32Error() != 0 /* catches ERROR_NOT_ALL_ASSIGNED (1300) */)
        throw new InvalidOperationException(
          "AdjustTokenPrivileges failed: " + Marshal.GetLastWin32Error());
      int cmd = 4; // MemoryPurgeStandbyList
      int status = NtSetSystemInformation(80 /* SystemMemoryListInformation */, ref cmd, 4);
      if (status != 0)
        throw new InvalidOperationException(
          "NtSetSystemInformation failed: 0x" + status.ToString("X8"));
    }
  }
}
'@
    }
    [ColdCache.Native]::PurgeStandbyList()
}

function Get-Stats([double[]]$values) {
    $mean = ($values | Measure-Object -Average).Average
    $sumSq = 0.0
    foreach ($v in $values) { $sumSq += ($v - $mean) * ($v - $mean) }
    $stddev = if ($values.Count -gt 1) { [Math]::Sqrt($sumSq / ($values.Count - 1)) } else { 0.0 }
    [pscustomobject]@{
        Mean   = $mean
        StdDev = $stddev
        Min    = ($values | Measure-Object -Minimum).Minimum
        Max    = ($values | Measure-Object -Maximum).Maximum
    }
}

Assert-Admin

if ($Cleanup) {
    Dismount-Bench
    if (Test-Path $VhdxPath) {
        Remove-Item $VhdxPath -Force
        Write-Host "Deleted $VhdxPath"
    } else {
        Write-Host "Nothing to clean up at $VhdxPath"
    }
    Remove-Item "$VhdxPath.populated" -Force -ErrorAction SilentlyContinue
    return
}

if ($Rebuild -and (Test-Path $VhdxPath)) {
    Dismount-Bench
    Remove-Item $VhdxPath -Force
    Remove-Item "$VhdxPath.populated" -Force -ErrorAction SilentlyContinue
}

# --- Build ---
if (-not $Exe) { $Exe = Join-Path $repoRoot 'target\release\scan_only.exe' }
if (-not $NoBuild -and -not $PSBoundParameters.ContainsKey('Exe')) {
    Write-Host 'Building scan_only (release) ...'
    Push-Location $repoRoot
    try {
        cargo build --release --features internal-tools --bin scan_only
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
    } finally { Pop-Location }
}
if (-not (Test-Path $Exe)) { throw "scan binary not found: $Exe" }

# --- Fixture ---
# The marker file distinguishes a populated fixture from one whose creation
# or population was interrupted.
$marker = "$VhdxPath.populated"
$needPopulate = -not ((Test-Path $VhdxPath) -and (Test-Path $marker))
if ($needPopulate -and (Test-Path $VhdxPath)) {
    Write-Host 'Existing VHDX was never fully populated; recreating.'
    Dismount-Bench
    Remove-Item $VhdxPath -Force
    Remove-Item $marker -Force -ErrorAction SilentlyContinue
}
if ($needPopulate) { New-BenchVhdx }

try {
    if ($needPopulate) {
        $root = Mount-Bench
        Initialize-Fixture $root
        Set-Content -Path $marker -Value (Get-Date -Format o)
    } else {
        Write-Host "Reusing existing fixture: $VhdxPath (use -Rebuild to regenerate)"
        $root = Mount-Bench
    }

    Write-Host ''
    Write-Host '=== Cold-Cache Scan Benchmark ==='
    Write-Host "  Binary:  $Exe"
    Write-Host "  Fixture: $VhdxPath -> $root"
    Write-Host "  Runs:    $Runs (dismount/remount before each$(if ($PurgeStandby) { ', standby list purged' }))"
    Write-Host ''

    $times = New-Object System.Collections.Generic.List[double]
    $outputs = New-Object System.Collections.Generic.List[string]

    for ($i = 1; $i -le $Runs; $i++) {
        Dismount-Bench
        if ($PurgeStandby) { Invoke-StandbyPurge }
        $root = Mount-Bench

        $sw = [Diagnostics.Stopwatch]::StartNew()
        $out = & $Exe $root
        $sw.Stop()
        if ($LASTEXITCODE -ne 0) { throw "scan_only failed (exit $LASTEXITCODE): $out" }

        $secs = $sw.Elapsed.TotalSeconds
        $times.Add($secs)
        $outputs.Add(($out | Select-Object -First 1))
        Write-Host ('  cold run {0}/{1}: {2,8:f3}s   {3}' -f $i, $Runs, $secs, $outputs[-1])
    }

    if ($AlsoHot) {
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $out = & $Exe $root
        $sw.Stop()
        Write-Host ('  hot re-scan:  {0,8:f3}s   {1}' -f $sw.Elapsed.TotalSeconds, ($out | Select-Object -First 1))
    }

    if (($outputs | Select-Object -Unique).Count -gt 1) {
        Write-Warning 'Scan output differed between runs (file counts not identical):'
        $outputs | Select-Object -Unique | ForEach-Object { Write-Warning "  $_" }
    }

    $stats = Get-Stats $times.ToArray()
    Write-Host ''
    Write-Host '=== Results (cold) ==='
    Write-Host ('  mean   {0,8:f3}s' -f $stats.Mean)
    Write-Host ('  stddev {0,8:f3}s ({1:f1}%)' -f $stats.StdDev, ($(if ($stats.Mean) { 100 * $stats.StdDev / $stats.Mean } else { 0 })))
    Write-Host ('  min    {0,8:f3}s' -f $stats.Min)
    Write-Host ('  max    {0,8:f3}s' -f $stats.Max)
    Write-Host '======================'
} finally {
    Dismount-Bench
}
