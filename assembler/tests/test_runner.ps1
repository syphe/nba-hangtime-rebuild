# test_runner.ps1 - Automated integration test runner for the TMS34010 assembler.
# Assembles each .asm file in tests/cases with --dump and checks that all
# verification checks pass (little-endian layout, branch offsets, immediate
# operands, .WORD values). Exit code is the number of failing tests.

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $root "..\target\debug\tms34010-asm.exe"

if (-not (Test-Path $exe)) {
    Write-Host "Assembler not found. Run 'cargo build' first." -ForegroundColor Red
    exit 1
}

$tests = Get-ChildItem -Path (Join-Path $root "cases") -Filter "*.asm" | Sort-Object Name

$pass = 0
$fail = 0

foreach ($t in $tests) {
    $outBin = Join-Path $root ("out_" + $t.BaseName + ".bin")
    Write-Host "`n=== $($t.Name) ===" -ForegroundColor Cyan

    $output = & $exe --input $t.FullName --output $outBin --interleave-ways 1 --dump 2>&1
    $code = $LASTEXITCODE

    $checks = $output | Select-String -Pattern "^\s+\[(PASS|FAIL)\]"
    foreach ($c in $checks) { Write-Host $c.Line }

    if ($code -eq 0) {
        Write-Host "PASS: $($t.Name)" -ForegroundColor Green
        $pass++
    } else {
        Write-Host "FAIL: $($t.Name) (exit $code)" -ForegroundColor Red
        $fail++
    }
}

Write-Host "`n====================================="
Write-Host "Total: $pass passed, $fail failed" -ForegroundColor ($(if ($fail -eq 0) { "Green" } else { "Red" }))
exit $fail
