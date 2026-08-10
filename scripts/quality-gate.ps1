[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$minLineCoverage = 80
$qualityDir = Join-Path (Get-Location) "target\quality"
$lcovPath = Join-Path $qualityDir "lcov.info"
$clippyPath = Join-Path $qualityDir "clippy.json"
$clippyErrorPath = Join-Path $qualityDir "clippy.stderr"

New-Item -ItemType Directory -Force -Path $qualityDir | Out-Null

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)][string]$File,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    & $File @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$File $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

Invoke-Checked "cargo" @("fmt", "--all", "--", "--check")
Invoke-Checked "git" @("diff", "--check")
Invoke-Checked "cargo" @("test", "--workspace", "--locked")
Invoke-Checked "cargo" @("clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings")
$clippyErrorAction = $ErrorActionPreference
$ErrorActionPreference = "Continue"
& cargo clippy --workspace --all-targets --locked --message-format=json -- -D warnings 1> $clippyPath 2> $clippyErrorPath
$clippyExitCode = $LASTEXITCODE
$ErrorActionPreference = $clippyErrorAction
if ($clippyExitCode -ne 0) {
    throw "cargo clippy JSON report failed with exit code $clippyExitCode"
}
Invoke-Checked "cargo" @("build", "--workspace", "--locked")

& cargo llvm-cov --version *> $null
if ($LASTEXITCODE -ne 0) {
    throw "cargo-llvm-cov is required; install it before running the quality gate"
}

Invoke-Checked "cargo" @(
    "llvm-cov", "--workspace", "--all-features", "--locked", "--lcov",
    "--output-path", $lcovPath, "--fail-under-lines", $minLineCoverage
)

if (-not (Test-Path -LiteralPath $lcovPath) -or (Get-Item -LiteralPath $lcovPath).Length -eq 0) {
    throw "coverage report missing or empty: $lcovPath"
}
if (-not (Test-Path -LiteralPath $clippyPath) -or (Get-Item -LiteralPath $clippyPath).Length -eq 0) {
    throw "Clippy report missing or empty: $clippyPath"
}

if (-not (Get-Command "sonar-scanner" -ErrorAction SilentlyContinue)) {
    throw "sonar-scanner is required; the quality gate cannot skip Sonar"
}

$sonarToken = [Environment]::GetEnvironmentVariable("SONAR_TOKEN")
if ([string]::IsNullOrWhiteSpace($sonarToken)) {
    throw "SONAR_TOKEN is required; the quality gate cannot run unauthenticated"
}

$sonarHost = [Environment]::GetEnvironmentVariable("SONAR_HOST_URL")
if ([string]::IsNullOrWhiteSpace($sonarHost)) {
    $sonarHost = "https://sonarcloud.io"
}

Invoke-Checked "sonar-scanner" @(
    "-Dsonar.host.url=$sonarHost",
    "-Dsonar.qualitygate.wait=true",
    "-Dsonar.qualitygate.timeout=300"
)

Write-Output "Quality gate passed: line coverage >= $minLineCoverage% and Sonar quality gate passed."
