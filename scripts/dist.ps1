<#
.SYNOPSIS
  本地打包 RidgeCode 当前平台的独立分发产物(Windows)。
.DESCRIPTION
  cargo build --release 后,把 ridgecode.exe + README + 安装脚本打成
  dist\ridgecode-<target>.zip,并生成 .sha256。产物零依赖、双击/命令行可直接跑。
  跨平台(Linux/macOS)产物由 CI 出:打 v* 标签触发 .github/workflows/release.yml。
#>
[CmdletBinding()]
param([string]$OutDir = "dist")
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

$target = (& rustc -vV | Select-String '^host:').ToString().Split(' ')[1]
$name = "ridgecode-$target"
Write-Host "构建 release（$target）…" -ForegroundColor Cyan
$build_error_action = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$build_output = @(& cargo build --release --locked --bin ridgecode -p agent 2>&1)
$build_exit_code = $LASTEXITCODE
$ErrorActionPreference = $build_error_action
if ($build_exit_code -ne 0) {
  $build_output | ForEach-Object { Write-Host $_ -ForegroundColor Red }
  throw "cargo build 失败"
}

$stage = Join-Path $env:TEMP $name
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Copy-Item "$root\target\release\ridgecode.exe" $stage
Copy-Item "$root\README.md" $stage
Copy-Item "$root\scripts\install.ps1" $stage

New-Item -ItemType Directory -Force -Path (Join-Path $root $OutDir) | Out-Null
$zip = Join-Path $root "$OutDir\$name.zip"
Remove-Item -Force $zip -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\*" -DestinationPath $zip
(Get-FileHash $zip -Algorithm SHA256).Hash.ToLower() + "  $name.zip" |
  Out-File -Encoding ascii "$zip.sha256"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue

Write-Host "[OK] $zip" -ForegroundColor Green
Write-Host "     $zip.sha256" -ForegroundColor Green
