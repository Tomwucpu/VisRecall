# VisRecall release asset builder.
#
# 产出两个 NSIS 安装包与一份 SHA256SUMS.txt：
#   VisRecall_<版本>_<arch>-setup.exe          轻量版，缺少 WebView2 时联网安装（基座配置）
#   VisRecall_<版本>_<arch>-setup-offline.exe  离线版，内置 WebView2 Evergreen 安装器
# 两者都随包携带 models/ 与 ONNX Runtime。
#
# Usage:
#   scripts/build-release.ps1
#   scripts/build-release.ps1 -Target x86_64-pc-windows-msvc -OutDir target/release-assets
#
# Exit code 0 = 两个资产与校验文件均已生成，1 = 构建失败或产物缺失。

[CmdletBinding()]
param(
    [string]$Target = 'x86_64-pc-windows-msvc',
    [string]$OutDir = 'target/release-assets'
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $repoRoot

$arch = if ($Target -like 'x86_64*') { 'x64' } elseif ($Target -like 'aarch64*') { 'arm64' } else { throw "unsupported target: $Target" }
$version = (Get-Content -LiteralPath 'src-tauri/tauri.conf.json' -Raw | ConvertFrom-Json).version
if (-not $version) { throw 'cannot read version from src-tauri/tauri.conf.json' }

$tauriCli = 'node_modules/@tauri-apps/cli/tauri.js'
if (-not (Test-Path -LiteralPath $tauriCli)) { throw "tauri cli not found: $tauriCli (run pnpm install)" }

$bundleDir = Join-Path $repoRoot "target/$Target/release/bundle/nsis"
$litePath = Join-Path $OutDir "VisRecall_${version}_${arch}-setup.exe"
$offlinePath = Join-Path $OutDir "VisRecall_${version}_${arch}-setup-offline.exe"
$sumsPath = Join-Path $OutDir 'SHA256SUMS.txt'

function Invoke-Build([string[]]$ExtraArgs) {
    $arguments = @($tauriCli, 'build', '--target', $Target, '--bundles', 'nsis') + $ExtraArgs + @('--', '--locked')
    Write-Host "> node $($arguments -join ' ')" -ForegroundColor Cyan
    & node @arguments
    if ($LASTEXITCODE -ne 0) { throw "tauri build failed with exit code $LASTEXITCODE" }
}

# NSIS 产物名由 productName/version 决定，两种模式会写同一个文件名；
# 每次构建后立即拷到 OutDir 并删除，避免下一个模式覆盖上一份。
function Save-Artifact([string]$Destination) {
    $source = Join-Path $bundleDir "VisRecall_${version}_${arch}-setup.exe"
    if (-not (Test-Path -LiteralPath $source)) {
        $candidate = Get-ChildItem -LiteralPath $bundleDir -Filter '*-setup.exe' |
            Sort-Object LastWriteTime -Descending |
            Select-Object -First 1
        if (-not $candidate) { throw "installer not found in $bundleDir" }
        $source = $candidate.FullName
    }
    Copy-Item -LiteralPath $source -Destination $Destination -Force
    Remove-Item -LiteralPath $source -Force
    $size = [math]::Round((Get-Item -LiteralPath $Destination).Length / 1MB, 1)
    Write-Host "  saved $([System.IO.Path]::GetFileName($Destination)) ($size MB)" -ForegroundColor Green
}

if (-not (Test-Path -LiteralPath $OutDir)) {
    New-Item -ItemType Directory -Path $OutDir | Out-Null
} else {
    Remove-Item -LiteralPath $litePath, $offlinePath, $sumsPath -Force -ErrorAction SilentlyContinue
}

Write-Host "== 1/2 轻量版（WebView2 按需联网安装）" -ForegroundColor Yellow
Invoke-Build @()
Save-Artifact $litePath

Write-Host '== 2/2 离线版（内置 WebView2 安装器）' -ForegroundColor Yellow
Invoke-Build @('--config', 'src-tauri/tauri.offline.conf.json')
Save-Artifact $offlinePath

$lines = foreach ($path in @($litePath, $offlinePath)) {
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $([System.IO.Path]::GetFileName($path))"
}
$lines | Set-Content -LiteralPath $sumsPath -Encoding ascii

Write-Host "`nassets:" -ForegroundColor Green
$lines | ForEach-Object { Write-Host "  $_" }
Write-Host "written to $OutDir"
