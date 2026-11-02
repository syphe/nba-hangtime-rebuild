param(
    [switch]$Clean,
    [switch]$ShowVerbose,
    [switch]$ObjectFormat
)

$ErrorActionPreference = "Stop"

$assemblerDir = $PSScriptRoot
$repoRoot = Split-Path -Parent $assemblerDir
$sourceDir = Join-Path $repoRoot "SRC"
$outputDir = Join-Path $assemblerDir "build"
$exe = Join-Path $assemblerDir "target\debug\tms34010-asm.exe"
$assemblerSources = @(Get-ChildItem -LiteralPath (Join-Path $assemblerDir "src") -Filter "*.rs" -File | ForEach-Object { $_.FullName })

if ($Clean) {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $outputDir
}

New-Item -ItemType Directory -Force $outputDir | Out-Null

Write-Host "Building assembler..." -ForegroundColor Cyan
Push-Location $assemblerDir
try {
    & cargo build
    $cargoExitCode = $LASTEXITCODE
}
finally {
    Pop-Location
}
if ($cargoExitCode -ne 0) { exit $cargoExitCode }

$modules = @(
    "NDSP1", "MAIN", "UTIL", "PAL", "MPROC", "BAKGND", "BGNDTBL", "BGNDPAL", "IMGTBL", "IMGPAL",
    "BB", "BB3", "PLYR", "PLYR2", "PLYRAT", "PLYRSEQ", "PLYRDSEQ", "PLYRSEQ2", "PLYR3",
    "PLYRAT2", "PLYRLSEQ", "PLYRRSEQ", "SCORE", "SCORE2", "BB2", "BB4", "DRONE", "SOUNDS",
    "PATCH", "TEXT", "ATTRACT", "PLYRSTND", "SELECT", "SELECT2", "STRING", "RECORD", "SPEECH",
    "ADJUST", "AUDIT", "TEST", "DIAG", "MENU", "HSTD", "MAKEPLR", "MAKEPLR2", "SCREEN",
    "SELECT3", "SELECT4", "PLYRAT3", "PLYRDSQ2", "UNZIP"
)

$dependencies = @{
    NDSP1 = @("world.equ", "disp.equ", "court.tbl")
    MAIN = @("sys.equ")
    PAL = @("plyrhd6.tbl", "plyrhd6.glo", "imgpal8.asm", "mugshot.tbl", "mugshot.glo")
    BB = @("world.equ", "bbsec.equ", "court.tbl", "imgpal2.asm", "sys.equ", "imgpal7.asm")
    PLYR = @("world.equ", "plyr.equ", "disp.equ", "court.tbl", "imgtbl2.glo", "sys.equ")
    PLYR2 = @("world.equ", "plyr.equ", "ball.tbl", "sys.equ")
    PLYR3 = @("world.equ", "plyr.equ", "plyrhd2.tbl", "imgtbl2.glo")
    PLYRAT = @("plyrhd.tbl", "plyr.equ")
    PLYRAT2 = @("plyrhd.tbl", "plyr.equ", "sys.equ")
    PLYRAT3 = @("imgtbl.asm")
    PLYRSEQ = @("plyrseq.tbl", "plyr.equ", "stand.tbl")
    PLYRRSEQ = @("plyrseq.tbl", "plyr.equ", "stand.tbl")
    PLYRSEQ2 = @("plyrseq2.tbl", "plyr.equ", "pass.tbl", "flail.tbl")
    PLYRDSEQ = @("plyrdseq.tbl", "plyrdsq2.tbl", "plyr.equ")
    PLYRDSQ2 = @("plyrdseq.tbl", "plyrdsq2.tbl", "plyr.equ")
    PLYRLSEQ = @("plyr.equ")
    PLYRSTND = @("stand.tbl", "imgtbl2.glo", "plyrhd3.tbl")
    BB2 = @("plyrseq.tbl", "plyrdseq.tbl", "sys.equ")
    BB3 = @("world.equ")
    BB4 = @("plyrhd.tbl", "plyrseq3.tbl")
    SCORE = @("world.equ", "leds.tbl", "arrow.tbl", "sys.equ")
    SCORE2 = @("world.equ", "imgtbl2.glo", "sys.equ")
    DRONE = @("world.equ", "plyr.equ")
    RECORD = @("world.equ")
    AUDIT = @("link.equ", "menu.equ")
    HSTD = @("link.equ")
    MENU = @("link.equ", "menu.equ", "imgtbl2.glo")
    TEST = @("link.equ", "menu.equ")
    ATTRACT = @("imgtbl.asm", "imgtbl2.glo")
    MAKEPLR = @("imgtbl.asm", "imgtbl2.glo")
    MAKEPLR2 = @("imgtbl.asm", "imgtbl2.glo")
    SCREEN = @("imgtbl.asm", "imgtbl2.glo")
    SELECT = @("mugshot.tbl", "imgtbl2.glo")
    SELECT2 = @("mugshot.tbl", "imgtbl2.glo", "sys.equ")
    SELECT3 = @("imgtbl.asm", "imgtbl2.glo")
    SELECT4 = @("imgtbl.asm", "imgtbl2.glo")
    DIAG = @("imgtbl2.glo")
    SPEECH = @("imgtbl2.glo")
    ADJUST = @("menu.equ")
    MPROC = @("mproc.equ")
    IMGTBL = @("imgtbl.glo", "fonttbl.glo", "roboimg.glo", "bgndtbl.glo")
    TEXT = @("text.tbl", "mugshot.tbl", "plyrhd5.glo", "plyrhd5.tbl", "imgpal4.asm")
    UNZIP = @("bbvda.tbl")
}

function Find-SourceFile([string]$Name) {
    Get-ChildItem -LiteralPath $sourceDir -File |
        Where-Object { $_.Name.Equals($Name, [System.StringComparison]::OrdinalIgnoreCase) } |
        Select-Object -First 1
}

$failed = 0
foreach ($module in $modules) {
    $source = Find-SourceFile "$module.ASM"
    if (-not $source) {
        Write-Host "SKIP: $module.ASM is not present" -ForegroundColor DarkYellow
        continue
    }

    $object = Join-Path $outputDir "$module.OBJ"
    $inputs = @($source.FullName, $exe)
    $inputs += $assemblerSources
    foreach ($dependency in $dependencies[$module]) {
        $dependencyFile = Find-SourceFile $dependency
        if ($dependencyFile) { $inputs += $dependencyFile.FullName }
    }

    $needsBuild = -not (Test-Path $object)
    if (-not $needsBuild) {
        $objectTime = (Get-Item $object).LastWriteTimeUtc
        foreach ($input in $inputs) {
            if ((Get-Item $input).LastWriteTimeUtc -gt $objectTime) {
                $needsBuild = $true
                break
            }
        }
    }
    if (-not $needsBuild) {
        Write-Host "UP TO DATE: $module.OBJ"
        continue
    }

    Write-Host "ASSEMBLING: $module.ASM -> $module.OBJ" -ForegroundColor Green
    $arguments = @("--input", $source.FullName, "--output", $object, "--interleave-ways", "1")
    if ($ShowVerbose) { $arguments += "--verbose" }
    if ($ObjectFormat) { $arguments += "--object-format" }
    & $exe @arguments
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $module.ASM (exit $LASTEXITCODE)" -ForegroundColor Red
        $failed++
    }
}

if ($failed -ne 0) {
    Write-Host "Assembly build failed: $failed module(s)" -ForegroundColor Red
    exit 1
}

if ($ObjectFormat) {
    Write-Host "LINKING: relocatable objects -> BB.bin" -ForegroundColor Cyan
    Push-Location $assemblerDir
    try {
        $objects = foreach ($module in $modules) {
            $object = Join-Path $outputDir "$module.OBJ"
            if (Test-Path $object) { $object }
        }
        & python link.py @objects --cmd-file (Join-Path $sourceDir "BB.CMD") `
            --output (Join-Path $outputDir "BB.bin") `
            --coff-output (Join-Path $outputDir "BB.OUT")
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        & python rom_split.py (Join-Path $outputDir "BB.bin") `
            --output-dir (Join-Path $outputDir "ROM") `
            --reference (Join-Path $repoRoot "BACKUP\XCODE101L\ROM\ROM_MINE.bin")
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
    finally {
        Pop-Location
    }
}

Write-Host "Assembly build completed: $outputDir" -ForegroundColor Green
