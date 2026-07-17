<#
.SYNOPSIS
  RidgeCode 安装器(Windows)—— 零 cargo、零源码,只需一个独立 .exe。
.DESCRIPTION
  在线装最新版:  irm https://raw.githubusercontent.com/MySetsuna/ridge-code/main/scripts/install.ps1 | iex
  指定版本:      &([scriptblock]::Create((irm .../install.ps1))) -Version v0.2.0
  装本地已构建:  .\scripts\install.ps1 -Local .\target\release\ridgecode.exe
  装到 $env:LOCALAPPDATA\Programs\ridgecode,并把该目录加入「用户 PATH」(新终端生效)。
.PARAMETER Version
  发布版本 tag(默认 latest)。
.PARAMETER Local
  改为安装一个本地已构建的 ridgecode.exe(不联网、不需 cargo)。
.PARAMETER Dir
  自定义安装目录。
#>
[CmdletBinding()]
param(
  [string]$Version = "latest",
  [string]$Local = "",
  [string]$Dir = "$env:LOCALAPPDATA\Programs\ridgecode"
)
$ErrorActionPreference = "Stop"
$Repo = "MySetsuna/ridge-code"
$Bin = "ridgecode.exe"

New-Item -ItemType Directory -Force -Path $Dir | Out-Null
$dest = Join-Path $Dir $Bin

if ($Local) {
  if (-not (Test-Path $Local)) { throw "本地文件不存在: $Local" }
  Copy-Item -Force $Local $dest
} else {
  # 仅支持 x86_64-msvc 发布产物(与 release.yml 的 Windows 目标一致)。
  $target = "x86_64-pc-windows-msvc"
  $base = "https://github.com/$Repo/releases"
  $url = if ($Version -eq "latest") { "$base/latest/download/ridgecode-$target.zip" }
         else { "$base/download/$Version/ridgecode-$target.zip" }
  $tmp = Join-Path $env:TEMP ("ridge-" + [guid]::NewGuid().ToString("N"))
  New-Item -ItemType Directory -Force -Path $tmp | Out-Null
  try {
    Write-Host "下载 $url"
    $zip = Join-Path $tmp "a.zip"
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $exe = Get-ChildItem -Path $tmp -Recurse -Filter $Bin | Select-Object -First 1
    if (-not $exe) { throw "归档里没找到 $Bin" }
    Copy-Item -Force $exe.FullName $dest
  } finally { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
}
Write-Host "[OK] 已安装: $dest" -ForegroundColor Green

# 把安装目录加入「用户 PATH」(幂等;新终端生效)。
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$parts = @()
if ($userPath) { $parts = $userPath.Split(';') | Where-Object { $_ -ne "" } }
if ($parts -notcontains $Dir) {
  [Environment]::SetEnvironmentVariable("Path", (($parts + $Dir) -join ';'), "User")
  Write-Host "[OK] 已把 $Dir 加入用户 PATH（新开终端生效）" -ForegroundColor Green
} else {
  Write-Host "PATH 中已有 $Dir" -ForegroundColor DarkGray
}
# 当前会话即时可用
if (($env:Path -split ';') -notcontains $Dir) { $env:Path = "$env:Path;$Dir" }
Write-Host "现在可运行: ridgecode" -ForegroundColor Cyan
