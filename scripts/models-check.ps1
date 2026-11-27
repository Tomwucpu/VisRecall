# VisRecall model manifest checker (verifies or regenerates models/manifest.json).
#
# Usage:
#   scripts/models-check.ps1                # verify models/ against models/manifest.json
#   scripts/models-check.ps1 -Write         # (re)generate manifest.json from files in models/
#   scripts/models-check.ps1 -Dir <path>    # operate on another directory
#
# Exit code 0 = all checksums match, 1 = mismatch / missing file / bad manifest.

[CmdletBinding()]
param(
    [string]$Dir = (Join-Path $PSScriptRoot '..\models'),
    [switch]$Write
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $Dir -PathType Container)) {
    Write-Host "models dir not found: $Dir" -ForegroundColor Red
    exit 1
}

$manifestPath = Join-Path $Dir 'manifest.json'

function Get-Sha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

if ($Write) {
    $existing = $null
    if (Test-Path -LiteralPath $manifestPath) {
        # keep hand-written provenance notes across regeneration
        $existing = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    }
    $files = [ordered]@{}
    Get-ChildItem -LiteralPath $Dir -File |
        Where-Object { $_.Name -ne 'manifest.json' } |
        Sort-Object Name |
        ForEach-Object {
            $files[$_.Name] = Get-Sha256 $_.FullName
            Write-Host ("{0}  {1}" -f $files[$_.Name], $_.Name)
        }
    if ($files.Count -eq 0) {
        Write-Host 'no model files found' -ForegroundColor Red
        exit 1
    }
    $manifest = [ordered]@{ files = $files }
    if ($existing -and $existing.sources) { $manifest['sources'] = $existing.sources }
    if ($existing -and $existing.licenses) { $manifest['licenses'] = $existing.licenses }
    ($manifest | ConvertTo-Json -Depth 5) | Set-Content -LiteralPath $manifestPath -Encoding UTF8
    Write-Host "wrote $manifestPath"
    exit 0
}

if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    Write-Host "manifest not found: $manifestPath (run with -Write to generate)" -ForegroundColor Red
    exit 1
}

$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$names = @($manifest.files.PSObject.Properties.Name)
if ($names.Count -eq 0) {
    Write-Host 'manifest lists no files' -ForegroundColor Red
    exit 1
}

$failed = 0
foreach ($name in $names) {
    $path = Join-Path $Dir $name
    $expected = [string]$manifest.files.$name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        Write-Host ("MISSING  {0}" -f $name) -ForegroundColor Red
        $failed++
        continue
    }
    $actual = Get-Sha256 $path
    if ($actual -eq $expected.ToLowerInvariant()) {
        Write-Host ("OK       {0}  {1}" -f $actual, $name) -ForegroundColor Green
    }
    else {
        Write-Host ("MISMATCH {0}`n  expected {1}`n  actual   {2}" -f $name, $expected, $actual) -ForegroundColor Red
        $failed++
    }
}

if ($failed -gt 0) {
    Write-Host ("{0} of {1} file(s) failed" -f $failed, $names.Count) -ForegroundColor Red
    exit 1
}
Write-Host ("all {0} file(s) verified" -f $names.Count) -ForegroundColor Green
exit 0
