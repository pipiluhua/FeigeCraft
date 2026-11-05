<#
.SYNOPSIS
    一键构建 FeigeCraft 的 Windows 安装包。

.DESCRIPTION
    依次完成：读版本号 → cargo build --release → 用 Inno Setup 打成一个 exe。
    产物在 dist\FeigeCraft-<版本>-setup.exe。

.PARAMETER SkipBuild
    跳过 cargo build，直接用 target\release 里现成的 exe 打包（调试脚本用）。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1
#>
param(
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# ---- 版本号：以 Cargo.toml 的 [workspace.package] 为准，避免两处写死对不上 ----
$cargoToml = Get-Content (Join-Path $root "Cargo.toml") -Raw
$versionMatch = [regex]::Match($cargoToml, '(?m)^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) {
    throw "没能从 Cargo.toml 读出版本号"
}
$version = $versionMatch.Groups[1].Value
Write-Host "版本: $version" -ForegroundColor Cyan

# ---- 编译 ----
if (-not $SkipBuild) {
    Write-Host "编译 release..." -ForegroundColor Cyan
    # PowerShell 的 PATH 里不一定有 cargo（rustup / Git Bash 装的位置常常不在），自己找一次。
    $cargoExe = Get-Command cargo.exe -ErrorAction SilentlyContinue
    if (-not $cargoExe) {
        $guess = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
        if (Test-Path $guess) { $cargoExe = $guess }
    }
    if (-not $cargoExe) { throw "找不到 cargo.exe，请确认 Rust 工具链已安装" }
    & $cargoExe build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build 失败" }
}

# 只剩一个 exe：命令行能力已经合并进主程序，不再单独产出 feigecraft-cli.exe。
$exe = Join-Path $root "target\release\feigecraft.exe"
foreach ($f in @($exe)) {
    if (-not (Test-Path $f)) { throw "缺少 $f，先跑一次 cargo build --release" }
}

# ---- 找 ISCC：用户级安装、系统级安装、PATH 都试一遍 ----
$iscc = $null
$candidates = @(
    (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"),
    (Join-Path $env:ProgramFiles "Inno Setup 6\ISCC.exe"),
    (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe")
)
foreach ($c in $candidates) {
    if ($c -and (Test-Path $c)) { $iscc = $c; break }
}
if (-not $iscc) {
    $cmd = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($cmd) { $iscc = $cmd.Source }
}
if (-not $iscc) {
    throw "找不到 ISCC.exe。请先安装 Inno Setup 6：winget install --id JRSoftware.InnoSetup --exact"
}
Write-Host "ISCC: $iscc" -ForegroundColor DarkGray

# ---- 语言文件：Inno 官方不带简体中文，仓库里放了一份，缺了就补进去 ----
$langSrc = Join-Path $root "installer\ChineseSimplified.isl"
$langDst = Join-Path (Split-Path -Parent $iscc) "Languages\ChineseSimplified.isl"
if ((Test-Path $langSrc) -and -not (Test-Path $langDst)) {
    Copy-Item $langSrc $langDst -Force
    Write-Host "已把简体中文语言文件复制到 Inno Setup 目录" -ForegroundColor DarkGray
}

# ---- 打包 ----
New-Item -ItemType Directory -Force -Path (Join-Path $root "dist") | Out-Null
$iss = Join-Path $root "installer\feigecraft.iss"
& $iscc "/DMyAppVersion=$version" $iss
if ($LASTEXITCODE -ne 0) { throw "ISCC 编译失败（错误码 $LASTEXITCODE）" }

$out = Join-Path $root "dist\FeigeCraft-$version-setup.exe"
if (-not (Test-Path $out)) { throw "没有生成 $out" }

$sizeMB = [math]::Round((Get-Item $out).Length / 1MB, 1)
Write-Host ""
Write-Host "完成: $out ($sizeMB MB)" -ForegroundColor Green
