[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

function Add-Check {
    param(
        [System.Collections.Generic.List[object]]$Checks,
        [string]$Name,
        [bool]$Passed,
        [string]$Detail
    )
    $Checks.Add([pscustomobject]@{ name = $Name; passed = $Passed; detail = $Detail })
}

function Invoke-Quiet {
    param([string[]]$Command)
    $old = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $text = & $Command[0] $Command[1..($Command.Length - 1)] 2>$null
        [pscustomobject]@{ exit_code = $LASTEXITCODE; text = ($text -join "`n") }
    }
    finally { $ErrorActionPreference = $old }
}

function Find-Command {
    param([string]$Name)
    $found = Get-Command $Name -ErrorAction SilentlyContinue
    if ($null -eq $found -and $env:OS -eq 'Windows_NT') {
        # npm global bins are commonly exposed as .ps1 shims on Windows;
        # PATHEXT does not resolve PowerShell scripts from a bare name.
        $found = Get-Command "$Name.ps1" -ErrorAction SilentlyContinue
    }
    $found
}

$checks = [System.Collections.Generic.List[object]]::new()
foreach ($command in @('cargo', 'npm')) {
    $found = Get-Command $command -ErrorAction SilentlyContinue
    Add-Check $checks $command ($null -ne $found) $(if ($found) { $found.Source } else { 'command is not on PATH' })
}

$llvm = Get-Command 'cargo-llvm-cov' -ErrorAction SilentlyContinue
if ($null -eq $llvm) {
    Add-Check $checks 'cargo-llvm-cov' $false 'required command is not on PATH'
} else {
    # `cargo-llvm-cov` is a Cargo subcommand plugin.  Invoking the plugin
    # executable directly with only `--version` exits 1; probe the supported
    # public entry point instead so Windows preflight agrees with CI/shell.
    $result = Invoke-Quiet @('cargo', 'llvm-cov', '--version')
    $ok = $result.exit_code -eq 0 -and -not [string]::IsNullOrWhiteSpace($result.text)
    Add-Check $checks 'cargo-llvm-cov' $ok $(if ($ok) { $result.text.Trim() } else { 'command is present but not executable' })
}

$sonar = @('sonar-scanner', 'sonar-scanner-npm') |
    ForEach-Object { Find-Command $_ } |
    Where-Object { $null -ne $_ } |
    Select-Object -First 1
if ($null -eq $sonar) {
    Add-Check $checks 'sonar_scanner' $false 'sonar-scanner or sonar-scanner-npm is not on PATH'
} else {
    $result = Invoke-Quiet @($sonar.Name, '--version')
    $ok = $result.exit_code -eq 0
    Add-Check $checks 'sonar_scanner' $ok $(if ($ok) { $result.text.Trim() } else { 'scanner is present but not executable' })
}

$sonarToken = [Environment]::GetEnvironmentVariable('SONAR_TOKEN')
$hasToken = -not [string]::IsNullOrWhiteSpace($sonarToken)
Add-Check $checks 'sonar_token' $hasToken $(if ($hasToken) { 'present (value redacted)' } else { 'SONAR_TOKEN is not set' })

$ready = @($checks | Where-Object { -not $_.passed }).Count -eq 0
[pscustomobject]@{
    schema_version = 1
    kind = 'ridgecode_quality_preflight'
    ready = $ready
    checks = $checks
} | ConvertTo-Json -Depth 4

if (-not $ready) { exit 1 }
