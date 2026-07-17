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
# ---- 配置骨架:写 config.example.json 到安装目录 + 首次生成 ~/.ridge/config.json ----
# 注意:配置文件写成 UTF-8 无 BOM(serde_json 不吞 BOM,带 BOM 会解析失败)。
$noBom = New-Object System.Text.UTF8Encoding $false
$exampleJson = @'
{
  "provider": "openai",
  "model": "glm-4.6",
  "base_url": "https://open.bigmodel.cn/api/paas/v4",
  "api_key": "把你的 API Key 明文填这里即可直接启动;不想明文就删掉此行,改为设 RIDGE_API_KEY 环境变量",
  "budget_tokens": 200000,
  "skip_danger": false,
  "providers": [
    {
      "name": "kimi",
      "kind": "openai",
      "model": "kimi-k2",
      "base_url": "https://api.moonshot.cn/v1",
      "key_env": "MOONSHOT_KEY"
    }
  ],
  "mcp": [
    { "name": "notebooklm", "cmd": "notebooklm-mcp" }
  ]
}
'@
$examplePath = Join-Path $Dir "config.example.json"
[System.IO.File]::WriteAllText($examplePath, $exampleJson, $noBom)
Write-Host "[OK] 示例配置: $examplePath" -ForegroundColor Green

$cfgPath = if ($env:RIDGE_CONFIG) { $env:RIDGE_CONFIG } else { Join-Path $env:USERPROFILE ".ridge\config.json" }
$cfgDir = Split-Path -Parent $cfgPath
New-Item -ItemType Directory -Force -Path $cfgDir | Out-Null
if (-not (Test-Path $cfgPath)) {
  $initJson = @'
{
  "provider": "openai",
  "model": "glm-4.6",
  "base_url": "https://open.bigmodel.cn/api/paas/v4",
  "api_key": "",
  "budget_tokens": 200000,
  "skip_danger": false,
  "providers": [],
  "mcp": []
}
'@
  [System.IO.File]::WriteAllText($cfgPath, $initJson, $noBom)
  Write-Host "[OK] 已生成配置: $cfgPath  —— 填顶层 api_key（或设 RIDGE_API_KEY）即可启动真实 LLM" -ForegroundColor Green
} else {
  Write-Host "已有配置，未改动: $cfgPath（参照 $examplePath 补 api_key）" -ForegroundColor DarkGray
}

# 当前会话即时可用
if (($env:Path -split ';') -notcontains $Dir) { $env:Path = "$env:Path;$Dir" }
Write-Host "现在可运行: ridgecode" -ForegroundColor Cyan
