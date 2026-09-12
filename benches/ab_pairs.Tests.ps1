# Standalone regression checks: powershell -NoProfile -File benches/ab_pairs.Tests.ps1
# Also run with pwsh to cover both Windows argument-passing implementations.
$ErrorActionPreference = 'Stop'
# Expected native failures are checked through LASTEXITCODE, even when the
# caller enables this PowerShell 7 preference. Harmless on Windows PowerShell.
$PSNativeCommandUseErrorActionPreference = $false
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
    assert_eq!(path, &std::env::var("AB_TEST_EXPECTED_PATH").unwrap(), "bad path");
    assert_eq!(std::env::current_dir().unwrap(), std::path::PathBuf::from(
        std::env::var_os("AB_TEST_WORKING_DIR").unwrap()), "bad working directory");
    let exe = std::env::current_exe().unwrap();
    let is_b = exe.file_stem().unwrap() == "scan_b";
    let counter = exe.with_extension("count");
    let run = std::fs::read_to_string(&counter).unwrap_or_default()
        .trim().parse::<usize>().unwrap_or(0) + 1;
    std::fs::write(counter, run.to_string()).unwrap();
    if path.contains("stderr") {
        std::io::stderr().write_all(&vec![b'x'; 128 * 1024]).unwrap();
    }
    if path.contains("missing") {
        println!("no benchmark output");
        return;
    }
    let scan_ms = if is_b && path.contains("noisy-null") {
        [80.0, 125.0, 90.0, 10000.0 / 90.0][(run - 1) % 4]
    } else if is_b && path.contains("noisy-faster") {
        [70.0, 80.0, 90.0, 85.0][(run - 1) % 4]
    } else if is_b && path.contains("faster") { 80.0 }
      else if is_b && path.contains("slower") { 120.0 }
      else { 100.0 };
    let scan = if path.contains("zero") { "0.000".to_string() }
        else if path.contains("malformed") { "1.2.3".to_string() }
        else { format!("{scan_ms:.6}") };
    let files = if is_b && path.contains("files-mismatch") { 41 } else { 42 };
    let bytes = if is_b && (path.contains("bytes-mismatch")
        || (path.contains("late-mismatch") && run >= 4)) { 0 } else { 1234 };
    println!("BENCH scan_ms={scan} drop_ms=1.000 files={files} bytes={bytes}");
    if path.contains("failed") || (path.contains("late-failure") && is_b && run >= 4) {
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
[Threading.Thread]::CurrentThread.CurrentCulture = [Globalization.CultureInfo]::InvariantCulture
$env:AB_TEST_EXPECTED_PATH = $config.ExpectedPath
$env:AB_TEST_WORKING_DIR = $PSScriptRoot
# Deliberately diverge PowerShell's location from the process CWD.
[Environment]::CurrentDirectory = [IO.Path]::GetPathRoot($PSScriptRoot)
Set-Location -LiteralPath $PSScriptRoot
try {
    & $config.Harness -ExeA $config.ExeA -ExeB $config.ExeB -ScanPath $config.Path -Pairs $config.Pairs -Warmup $config.Warmup -Boot $config.Boot -CsvPath $config.Csv 2>&1
} catch {
    Write-Output $_
    exit 1
}
'@ | Set-Content -LiteralPath $runner

    $cases = @(
        @{ Name = 'identical / drive root'; Path = 'C:\'; Match = 'INCONCLUSIVE'; Success = $true },
        @{ Name = 'spaces / trailing slash'; Path = 'C:\fixture with spaces\'; Match = 'INCONCLUSIVE'; Success = $true },
        @{ Name = 'no trailing slash'; Path = 'C:\fixture with spaces'; Match = 'INCONCLUSIVE'; Success = $true },
        @{ Name = 'relative paths after cd'; Path = 'relative target'; Match = 'INCONCLUSIVE'; Success = $true; Relative = $true },
        @{ Name = 'faster'; Path = 'C:\faster\'; Match = 'significantly FASTER'; Success = $true },
        @{ Name = 'slower'; Path = 'C:\slower\'; Match = 'significantly SLOWER'; Success = $true },
        @{ Name = 'noisy null'; Path = 'C:\noisy-null\'; Match = 'INCONCLUSIVE'; Success = $true; Noisy = $true; Boot = 2000 },
        @{ Name = 'noisy faster'; Path = 'C:\noisy-faster\'; Match = 'significantly FASTER'; Success = $true; Noisy = $true; Boot = 2000 },
        @{ Name = 'bytes mismatch'; Path = 'C:\bytes-mismatch\'; Match = 'scan result mismatch'; Success = $false },
        @{ Name = 'files mismatch'; Path = 'C:\files-mismatch\'; Match = 'scan result mismatch'; Success = $false },
        @{ Name = 'missing output'; Path = 'C:\missing\'; Match = 'Failed to parse BENCH'; Success = $false },
        @{ Name = 'malformed output'; Path = 'C:\malformed\'; Match = 'Failed to parse BENCH'; Success = $false },
        @{ Name = 'zero duration'; Path = 'C:\zero\'; Match = 'Invalid BENCH timing'; Success = $false },
        @{ Name = 'nonzero exit with valid output'; Path = 'C:\failed\'; Match = 'exit 7'; Success = $false },
        @{ Name = 'stderr pipe capacity'; Path = 'C:\stderr\'; Match = 'INCONCLUSIVE'; Success = $true },
        @{ Name = 'failed warmup'; Path = 'C:\failed\'; Match = 'exit 7'; Success = $false; Warmup = 1 },
        @{ Name = 'late process failure preserves CSV'; Path = 'C:\late-failure\'; Match = 'exit 7'; Success = $false; SavedPairs = 3 },
        @{ Name = 'late mismatch preserves CSV'; Path = 'C:\late-mismatch\'; Match = 'scan result mismatch'; Success = $false; SavedPairs = 3 },
        @{ Name = 'too few pairs'; Path = 'C:\'; Match = 'Pairs'; Success = $false; Pairs = 1 },
        @{ Name = 'small even sample'; Path = 'C:\'; Match = 'Pairs'; Success = $false; Pairs = 4 },
        @{ Name = 'odd pair count'; Path = 'C:\'; Match = 'Pairs'; Success = $false; Pairs = 21 },
        @{ Name = 'negative warmup'; Path = 'C:\'; Match = 'Warmup'; Success = $false; Warmup = -1 },
        @{ Name = 'too few bootstrap samples'; Path = 'C:\'; Match = 'Boot'; Success = $false; Boot = 0 }
    )
    $caseIndex = 0
    foreach ($case in $cases) {
        $caseIndex++
        $params = @{ Pairs = 20; Warmup = 0; Boot = 100 }
        foreach ($key in @('Pairs', 'Warmup', 'Boot')) {
            if ($case.ContainsKey($key)) { $params[$key] = $case[$key] }
        }
        Set-Content -LiteralPath (Join-Path $testDir 'scan_a.count') -Value '0'
        Set-Content -LiteralPath (Join-Path $testDir 'scan_b.count') -Value '0'
        $csv = Join-Path $testDir "runs-$caseIndex.csv"
        # An existing output must be replaced, never mixed with this run.
        Set-Content -LiteralPath $csv -Value 'stale results'
        $inputA = $exeA; $inputB = $exeB; $inputCsv = $csv
        $expectedPath = $case.Path
        if ($case.Relative) {
            $inputA = '.\scan_a.exe'; $inputB = '.\scan_b.exe'
            $inputCsv = ".\runs-$caseIndex.csv"
            $expectedPath = Join-Path $testDir $case.Path
        }
        # Pass the case through JSON so this test's shell invocation does not
        # add its own quoting behavior to the native argument-passing check.
        @{
            Harness = $harness; ExeA = $inputA; ExeB = $inputB; Path = $case.Path
            ExpectedPath = $expectedPath
            Pairs = $params.Pairs; Warmup = $params.Warmup; Boot = $params.Boot; Csv = $inputCsv
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $testDir 'case.json')
        $output = & $shell -NoProfile -File $runner
        $code = $LASTEXITCODE
        $text = $output -join "`n"
        if (($code -eq 0) -ne $case.Success -or $text -notmatch $case.Match) {
            throw "FAIL $($case.Name) (exit $code): $text"
        }
        if ($case.Success) {
            $rows = @(Import-Csv -LiteralPath $csv)
            if ($rows.Count -ne $params.Pairs -or @($rows | Where-Object Order -eq 'AB').Count -ne ($params.Pairs / 2)) {
                throw "FAIL $($case.Name): incorrect pair count or balance"
            }
            if (@($rows | Where-Object { $_.Files -ne '42' -or $_.Bytes -ne '1234' }).Count -ne 0) {
                throw "FAIL $($case.Name): missing scan totals in CSV"
            }
            if ($case.Noisy) {
                $interval = [regex]::Match($text, '95% CI (-?[0-9.]+)% \.\. (-?[0-9.]+)%')
                if (-not $interval.Success) { throw "FAIL $($case.Name): missing confidence interval" }
                $lo = [double]$interval.Groups[1].Value
                $hi = [double]$interval.Groups[2].Value
                if ($lo -ge $hi -or ($hi - $lo) -lt 1) {
                    throw "FAIL $($case.Name): bootstrap interval must have nonzero width"
                }
                if ($case.Name -eq 'noisy null' -and -not ($lo -lt 0 -and $hi -gt 0)) {
                    throw 'FAIL noisy null: interval must strictly straddle zero'
                }
                if ($case.Name -eq 'noisy faster' -and $hi -ge 0) {
                    throw 'FAIL noisy faster: interval must be entirely below zero'
                }
                # Independent geometric-mean calculation from the saved data.
                $product = 1.0
                foreach ($row in $rows) { $product *= [double]$row.BScan / [double]$row.AScan }
                $expected = 100 * ([math]::Pow($product, 1.0 / $rows.Count) - 1)
                $change = [regex]::Match($text, 'scan time change\s+: (-?[0-9.]+)%')
                if (-not $change.Success -or [math]::Abs([double]$change.Groups[1].Value - $expected) -gt 0.01) {
                    throw "FAIL $($case.Name): incorrect geometric mean"
                }
            }
        } elseif ($case.SavedPairs) {
            $rows = @(Import-Csv -LiteralPath $csv)
            if ($rows.Count -ne $case.SavedPairs -or $text -match 'VERDICT:') {
                throw "FAIL $($case.Name): preserve completed pairs without reporting an overall verdict"
            }
            for ($i = 0; $i -lt $rows.Count; $i++) {
                if ([int]$rows[$i].Pair -ne ($i + 1) -or $rows[$i].Bytes -ne '1234') {
                    throw "FAIL $($case.Name): CSV includes an invalid pair"
                }
            }
        } elseif ($params.Pairs -ge 20 -and $params.Pairs % 2 -eq 0 -and $params.Warmup -ge 0 -and $params.Boot -ge 100) {
            if ((Get-Item -LiteralPath $csv).Length -ne 0) {
                throw "FAIL $($case.Name): failed first pair left stale CSV results"
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
