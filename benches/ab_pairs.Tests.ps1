# Standalone regression checks: powershell -NoProfile -File benches/ab_pairs.Tests.ps1
# Also run with pwsh to cover both Windows argument-passing implementations.
$ErrorActionPreference = 'Stop'
$harness = Join-Path $PSScriptRoot 'ab_pairs.ps1'
$shell = (Get-Process -Id $PID).Path
$testDir = Join-Path ([IO.Path]::GetTempPath()) ('ab-pairs-tests-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testDir | Out-Null

try {
    # A real native process exercises argument decoding, pipe handling, exit
    # status and parsing, while deterministic timings keep the checks stable.
    $source = Join-Path $testDir 'fake_scan.rs'
    @'
use std::io::Write;

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 2);
    let path = &args[1];
    assert!(path.starts_with("C:\\") && path.ends_with('\\'), "bad path: {path:?}");
    assert!(!path.contains('"'), "bad quoting: {path:?}");
    let exe = std::env::current_exe().unwrap();
    let is_b = exe.file_stem().unwrap() == "scan_b";
    if path.contains("stderr") {
        std::io::stderr().write_all(&vec![b'x'; 128 * 1024]).unwrap();
    }
    if path.contains("missing") {
        println!("no benchmark output");
        return;
    }
    let scan = if path.contains("zero") { "0.000" }
        else if path.contains("malformed") { "1.2.3" }
        else if is_b && path.contains("faster") { "80.000" }
        else if is_b && path.contains("slower") { "120.000" }
        else { "100.000" };
    let files = if is_b && path.contains("files-mismatch") { 41 } else { 42 };
    let bytes = if is_b && path.contains("bytes-mismatch") { 0 } else { 1234 };
    println!("BENCH scan_ms={scan} drop_ms=1.000 files={files} bytes={bytes}");
    if path.contains("failed") {
        eprintln!("intentional failure");
        std::process::exit(7);
    }
}
'@ | Set-Content -LiteralPath $source
    $exeA = Join-Path $testDir 'scan_a.exe'
    $exeB = Join-Path $testDir 'scan_b.exe'
    & rustc --edition 2024 $source -o $exeA
    if ($LASTEXITCODE -ne 0) { throw 'Failed to compile test fixture' }
    Copy-Item -LiteralPath $exeA -Destination $exeB

    $runner = Join-Path $testDir 'run.ps1'
    @'
$config = Get-Content -Raw (Join-Path $PSScriptRoot 'case.json') | ConvertFrom-Json
try {
    & $config.Harness -ExeA $config.ExeA -ExeB $config.ExeB -ScanPath $config.Path -Pairs $config.Pairs -Warmup $config.Warmup -Boot $config.Boot -CsvPath $config.Csv 2>&1
} catch {
    Write-Output $_
    exit 1
}
'@ | Set-Content -LiteralPath $runner

    $cases = @(
        @{ Name = 'identical / drive root'; Path = 'C:\'; Match = 'no significant scan-speed difference'; Success = $true },
        @{ Name = 'spaces / trailing slash'; Path = 'C:\fixture with spaces\'; Match = 'no significant scan-speed difference'; Success = $true },
        @{ Name = 'faster'; Path = 'C:\faster\'; Match = 'significantly FASTER'; Success = $true },
        @{ Name = 'slower'; Path = 'C:\slower\'; Match = 'significantly SLOWER'; Success = $true },
        @{ Name = 'bytes mismatch'; Path = 'C:\bytes-mismatch\'; Match = 'scan result mismatch'; Success = $false },
        @{ Name = 'files mismatch'; Path = 'C:\files-mismatch\'; Match = 'scan result mismatch'; Success = $false },
        @{ Name = 'missing output'; Path = 'C:\missing\'; Match = 'Failed to parse BENCH'; Success = $false },
        @{ Name = 'malformed output'; Path = 'C:\malformed\'; Match = 'Failed to parse BENCH'; Success = $false },
        @{ Name = 'zero duration'; Path = 'C:\zero\'; Match = 'Invalid BENCH timing'; Success = $false },
        @{ Name = 'nonzero exit with valid output'; Path = 'C:\failed\'; Match = 'exit 7'; Success = $false },
        @{ Name = 'stderr pipe capacity'; Path = 'C:\stderr\'; Match = 'no significant scan-speed difference'; Success = $true },
        @{ Name = 'failed warmup'; Path = 'C:\failed\'; Match = 'exit 7'; Success = $false; Warmup = 1 },
        @{ Name = 'too few pairs'; Path = 'C:\'; Match = 'Pairs'; Success = $false; Pairs = 1 },
        @{ Name = 'negative warmup'; Path = 'C:\'; Match = 'Warmup'; Success = $false; Warmup = -1 },
        @{ Name = 'too few bootstrap samples'; Path = 'C:\'; Match = 'Boot'; Success = $false; Boot = 0 }
    )
    foreach ($case in $cases) {
        $params = @{ Pairs = 4; Warmup = 0; Boot = 100 }
        foreach ($key in @('Pairs', 'Warmup', 'Boot')) {
            if ($case.ContainsKey($key)) { $params[$key] = $case[$key] }
        }
        $csv = Join-Path $testDir 'runs.csv'
        # Pass the case through JSON so this test's shell invocation does not
        # add its own quoting behavior to the native argument-passing check.
        @{
            Harness = $harness; ExeA = $exeA; ExeB = $exeB; Path = $case.Path
            Pairs = $params.Pairs; Warmup = $params.Warmup; Boot = $params.Boot; Csv = $csv
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $testDir 'case.json')
        $output = & $shell -NoProfile -File $runner
        $code = $LASTEXITCODE
        $text = $output -join "`n"
        if (($code -eq 0) -ne $case.Success -or $text -notmatch $case.Match) {
            throw "FAIL $($case.Name) (exit $code): $text"
        }
        if ($case.Success) {
            $rows = @(Import-Csv -LiteralPath $csv)
            if ($rows.Count -ne 4 -or @($rows | Where-Object Order -eq 'AB').Count -ne 2) {
                throw "FAIL $($case.Name): incorrect pair count or balance"
            }
            if (@($rows | Where-Object { $_.Files -ne '42' -or $_.Bytes -ne '1234' }).Count -ne 0) {
                throw "FAIL $($case.Name): missing scan totals in CSV"
            }
        }
        Write-Host "PASS $($case.Name)"
    }
} finally {
    $resolved = [IO.Path]::GetFullPath($testDir)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Test directory outside temp root: $resolved"
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
