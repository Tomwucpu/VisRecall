# OCR character accuracy (1 - CER) against manual annotations.
#
# Usage:
#   scripts/ocr-accuracy.ps1 -Ocr <dir> -Annotations <dir> [-Threshold 0.97] [-Report <md>]
#
# Inputs:
#   <Ocr dir>/<stem>.ocr.txt          produced by `cargo run -p visrecall-ai --example ocr_spike -- --images <dir> --out <Ocr dir>`
#   <Annotations dir>/<stem>.txt      manual ground truth for the same image (UTF-8)
#
# Metric: whitespace is stripped from both sides (layout is not under test), then
#   CER        = levenshtein_chars / len(ground_truth)
#   accuracy   = 1 - CER  (clamped to [0,1])
# The aggregate number is micro-averaged (total edits / total reference chars),
# which is the micro-averaged character accuracy metric (target >= 97% on 100-200 screenshots).
#
# Exit code 0 = aggregate accuracy >= threshold, 1 otherwise (or on missing inputs).

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Ocr,
    [Parameter(Mandatory = $true)][string]$Annotations,
    [double]$Threshold = 0.97,
    [string]$Report
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $Ocr -PathType Container)) {
    Write-Host "ocr dir not found: $Ocr" -ForegroundColor Red; exit 1
}
if (-not (Test-Path -LiteralPath $Annotations -PathType Container)) {
    Write-Host "annotations dir not found: $Annotations" -ForegroundColor Red; exit 1
}

function Normalize([string]$Text) {
    if ($null -eq $Text) { return '' }
    # drop all whitespace (incl. CR/LF); keep punctuation, digits and CJK as-is
    return ($Text -replace '\s', '')
}

function Get-CerDistance([string]$A, [string]$B) {
    # Levenshtein over chars, two rows of memory.
    if ($A.Length -eq 0) { return $B.Length }
    if ($B.Length -eq 0) { return $A.Length }
    $prev = New-Object int[] ($B.Length + 1)
    $cur = New-Object int[] ($B.Length + 1)
    for ($j = 0; $j -le $B.Length; $j++) { $prev[$j] = $j }
    for ($i = 1; $i -le $A.Length; $i++) {
        $cur[0] = $i
        $ca = $A[$i - 1]
        for ($j = 1; $j -le $B.Length; $j++) {
            $cost = if ($ca -eq $B[$j - 1]) { 0 } else { 1 }
            $del = $prev[$j] + 1
            $ins = $cur[$j - 1] + 1
            $sub = $prev[$j - 1] + $cost
            $m = $del
            if ($ins -lt $m) { $m = $ins }
            if ($sub -lt $m) { $m = $sub }
            $cur[$j] = $m
        }
        $tmp = $prev; $prev = $cur; $cur = $tmp
    }
    return $prev[$B.Length]
}

$files = Get-ChildItem -LiteralPath $Ocr -Filter '*.ocr.txt' -File | Sort-Object Name
if ($files.Count -eq 0) {
    Write-Host "no *.ocr.txt in $Ocr" -ForegroundColor Red; exit 1
}

$rows = @()
$totalEdits = 0
$totalChars = 0
$missing = 0

foreach ($file in $files) {
    $stem = $file.Name -replace '\.ocr\.txt$', ''
    $gtPath = Join-Path $Annotations "$stem.txt"
    if (-not (Test-Path -LiteralPath $gtPath -PathType Leaf)) {
        Write-Host ("MISSING annotation  {0}" -f $stem) -ForegroundColor Yellow
        $missing++
        continue
    }
    $hyp = Normalize (Get-Content -LiteralPath $file.FullName -Raw -Encoding UTF8)
    $ref = Normalize (Get-Content -LiteralPath $gtPath -Raw -Encoding UTF8)
    if ($ref.Length -eq 0) {
        Write-Host ("SKIP empty annotation  {0}" -f $stem) -ForegroundColor Yellow
        continue
    }
    $dist = Get-CerDistance $hyp $ref
    $cer = $dist / $ref.Length
    $acc = [Math]::Round([Math]::Max(0.0, 1.0 - $cer), 4)
    $totalEdits += $dist
    $totalChars += $ref.Length
    $rows += [pscustomobject]@{
        Image = $stem; RefChars = $ref.Length; Edits = $dist; Accuracy = $acc
    }
}

if ($rows.Count -eq 0) {
    Write-Host 'no comparable pairs (annotations missing entirely)' -ForegroundColor Red
    exit 1
}

$overall = [Math]::Round([Math]::Max(0.0, 1.0 - ($totalEdits / $totalChars)), 4)
$worst = $rows | Sort-Object Accuracy | Select-Object -First 3

$rows | Format-Table -AutoSize | Out-String -Width 120 | Write-Host
Write-Host ("images={0}  missing_annotations={1}  ref_chars={2}  edits={3}" -f $rows.Count, $missing, $totalChars, $totalEdits)
Write-Host ("aggregate accuracy = {0:P2} (threshold {1:P0})" -f $overall, $Threshold) -ForegroundColor (
    if ($overall -ge $Threshold) { 'Green' } else { 'Red' })
Write-Host 'worst 3:'
$worst | Format-Table -AutoSize | Out-String -Width 120 | Write-Host

if ($Report) {
    $lines = @(
        '# OCR accuracy report',
        '',
        "- ocr dir: ``$Ocr``",
        "- annotations dir: ``$Annotations``",
        "- images compared: $($rows.Count) (missing annotations: $missing)",
        "- reference chars: $totalChars, edits: $totalEdits",
        "- aggregate accuracy: $overall (threshold $Threshold)",
        '',
        '| image | ref chars | edits | accuracy |',
        '|---|---|---|---|'
    )
    foreach ($r in $rows) {
        $lines += "| $($r.Image) | $($r.RefChars) | $($r.Edits) | $($r.Accuracy) |"
    }
    Set-Content -LiteralPath $Report -Value ($lines -join "`n") -Encoding UTF8
    Write-Host "report written to $Report"
}

if ($overall -lt $Threshold) { exit 1 }
exit 0
