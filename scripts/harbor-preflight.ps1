[CmdletBinding()]
param(
    [string]$StoragePath = (Get-Location).Path,
    [ValidateRange(1, 1024)]
    [int]$MinimumFreeGb = 120,
    [switch]$SkipDocker,
    [switch]$SkipHarbor
)

$ErrorActionPreference = 'Stop'

function Add-Check {
    param(
        [System.Collections.Generic.List[object]]$Checks,
        [string]$Name,
        [bool]$Passed,
        [string]$Detail
    )
    $Checks.Add([pscustomobject]@{
            name   = $Name
            passed = $Passed
            detail = $Detail
        })
}

function Invoke-QuietVersion {
    param([string[]]$Command)
    $previousPreference = $ErrorActionPreference
    try {
        # PowerShell 7 can otherwise promote a native non-zero exit into a
        # terminating NativeCommandError before this diagnostic can emit JSON.
        $ErrorActionPreference = 'Continue'
        $text = & $Command[0] $Command[1..($Command.Length - 1)] 2>$null
        [pscustomobject]@{ exit_code = $LASTEXITCODE; text = ($text -join "`n") }
    }
    finally {
        $ErrorActionPreference = $previousPreference
    }
}

$checks = [System.Collections.Generic.List[object]]::new()
$root = [System.IO.Path]::GetPathRoot((Resolve-Path -LiteralPath $StoragePath).Path)
$drive = Get-PSDrive -Name $root.TrimEnd(':', '\')
$freeGb = [math]::Round($drive.Free / 1GB, 1)
Add-Check $checks 'storage' ($freeGb -ge $MinimumFreeGb) "${freeGb} GiB free on $root; require ${MinimumFreeGb} GiB"

$adapter = Join-Path (Split-Path -Parent $PSScriptRoot) 'eval/harbor/ridgecode_agent.py'
Add-Check $checks 'ridgecode_harbor_adapter' (Test-Path -LiteralPath $adapter -PathType Leaf) $adapter

$python = Get-Command python -ErrorAction SilentlyContinue
if ($null -eq $python) {
    Add-Check $checks 'python' $false 'python command is not on PATH'
}
else {
    $version = Invoke-QuietVersion @('python', '--version')
    if ($version.exit_code -ne 0) {
        Add-Check $checks 'python' $false 'python command is present but runtime is not executable'
    }
    else {
        $result = Invoke-QuietVersion @('python', '-c', "import ast, pathlib; ast.parse(pathlib.Path(r'$adapter').read_text(encoding='utf-8'))")
        $passed = $result.exit_code -eq 0
        $detail = if ($passed) { 'adapter syntax is valid' } else { 'adapter Python syntax check failed' }
        Add-Check $checks 'python_adapter_syntax' $passed $detail
    }
}

if ($SkipDocker) {
    Add-Check $checks 'docker' $true 'skipped by caller'
}
else {
    $docker = Get-Command docker -ErrorAction SilentlyContinue
    if ($null -eq $docker) {
        Add-Check $checks 'docker' $false 'docker command is not on PATH'
    }
    else {
        $result = Invoke-QuietVersion @('docker', 'version', '--format', '{{.Server.Version}}')
        $passed = $result.exit_code -eq 0 -and -not [string]::IsNullOrWhiteSpace($result.text)
        $detail = if ($passed) { "server=$($result.text.Trim())" } else { 'Docker engine is not reachable' }
        Add-Check $checks 'docker' $passed $detail
    }
}

if ($SkipHarbor) {
    Add-Check $checks 'harbor' $true 'skipped by caller'
}
else {
    $harbor = Get-Command harbor -ErrorAction SilentlyContinue
    if ($null -eq $harbor) {
        Add-Check $checks 'harbor' $false 'harbor command is not on PATH'
    }
    else {
        $result = Invoke-QuietVersion @('harbor', '--version')
        $passed = $result.exit_code -eq 0
        $detail = if ($passed) { $result.text.Trim() } else { 'harbor command failed' }
        Add-Check $checks 'harbor' $passed $detail
    }
}

$ready = @($checks | Where-Object { -not $_.passed }).Count -eq 0
[pscustomobject]@{
    schema_version = 1
    kind           = 'ridgecode_harbor_preflight'
    ready          = $ready
    checks         = $checks
} | ConvertTo-Json -Depth 4

if (-not $ready) {
    exit 1
}
