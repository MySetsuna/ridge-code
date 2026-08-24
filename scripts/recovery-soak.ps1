[CmdletBinding()]
param(
    [int]$TimeoutMs = 20000
)

$ErrorActionPreference = "Stop"
if ($TimeoutMs -lt 1000) {
    throw "TimeoutMs must be at least 1000"
}

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$qualityDir = [IO.Path]::GetFullPath((Join-Path $repoRoot "target\quality"))
New-Item -ItemType Directory -Force -Path $qualityDir | Out-Null

$runId = [Guid]::NewGuid().ToString("N")
$manifestPath = Join-Path $qualityDir "recovery-$runId.jsonl"
$firstStdoutPath = Join-Path $qualityDir "recovery-$runId-first.stdout.log"
$firstStderrPath = Join-Path $qualityDir "recovery-$runId-first.stderr.log"
$secondStdoutPath = Join-Path $qualityDir "recovery-$runId-second.stdout.log"
$secondStderrPath = Join-Path $qualityDir "recovery-$runId-second.stderr.log"
$createdPaths = @(
    $manifestPath,
    $firstStdoutPath,
    $firstStderrPath,
    $secondStdoutPath,
    $secondStderrPath
)

function Assert-QualityPath {
    param([Parameter(Mandatory = $true)][string]$Path)

    $root = ([IO.Path]::GetFullPath($qualityDir)).TrimEnd('\') + '\'
    $resolved = [IO.Path]::GetFullPath($Path)
    if (-not $resolved.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to touch path outside target/quality: $resolved"
    }
}

function Remove-CreatedPath {
    param([Parameter(Mandatory = $true)][string]$Path)

    Assert-QualityPath $Path
    if (Test-Path -LiteralPath $Path) {
        Remove-Item -LiteralPath $Path -Force
    }
}

function Read-CompleteManifestRecords {
    if (-not (Test-Path -LiteralPath $manifestPath)) {
        return @()
    }
    try {
        $text = [IO.File]::ReadAllText($manifestPath)
    } catch {
        return @()
    }
    $records = @()
    foreach ($line in ($text -split "`n")) {
        $trimmed = $line.Trim().TrimEnd("`r")
        if ([String]::IsNullOrWhiteSpace($trimmed)) {
            continue
        }
        try {
            $record = $trimmed | ConvertFrom-Json
        } catch {
            continue
        }
        if ($record.version -eq 1 -and -not [String]::IsNullOrWhiteSpace([string]$record.fingerprint) -and $null -ne $record.result) {
            $records += $record
        }
    }
    return @($records)
}

function Start-EvalProcess {
    param(
        [Parameter(Mandatory = $true)][string]$StdoutPath,
        [Parameter(Mandatory = $true)][string]$StderrPath
    )

    $binary = Join-Path $repoRoot "target\debug\ridgecode-eval.exe"
    if (-not (Test-Path -LiteralPath $binary)) {
        throw "ridgecode-eval binary missing: $binary"
    }
    $escapedManifest = $manifestPath.Replace('"', '\"')
    $arguments = "--json --fail-on-unapproved --concurrency 1 --timeout-ms 10000 --recovery-fixture --manifest `"$escapedManifest`""
    $startInfo = New-Object Diagnostics.ProcessStartInfo
    $startInfo.FileName = $binary
    $startInfo.Arguments = $arguments
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        throw "failed to start ridgecode-eval"
    }
    return [pscustomobject]@{
        process = $process
        stdout = $process.StandardOutput.ReadToEndAsync()
        stderr = $process.StandardError.ReadToEndAsync()
        stdoutPath = $StdoutPath
        stderrPath = $StderrPath
    }
}

function Save-EvalOutput {
    param([Parameter(Mandatory = $true)]$Run)

    try {
        [IO.File]::WriteAllText($Run.stdoutPath, [string]$Run.stdout.Result)
    } catch {
        [IO.File]::WriteAllText($Run.stdoutPath, "")
    }
    try {
        [IO.File]::WriteAllText($Run.stderrPath, [string]$Run.stderr.Result)
    } catch {
        [IO.File]::WriteAllText($Run.stderrPath, "")
    }
}

function Stop-EvalProcessTree {
    param([Parameter(Mandatory = $true)]$Run)

    $process = $Run.process
    if ($process.HasExited) {
        return
    }

    $useTaskkill = $false
    try {
        # Process.Kill(bool) is available on newer .NET runtimes.
        $process.Kill($true)
    } catch [System.Management.Automation.MethodException] {
        $useTaskkill = $true
    } catch {
        # A process can exit between HasExited and Kill; that race is success.
        if ($process.HasExited) {
            return
        }
        throw
    }

    if ($useTaskkill) {
        # Windows PowerShell/.NET Framework lacks the overload; taskkill keeps
        # the same exact PID boundary while also terminating descendants.
        $taskkillErrorAction = $ErrorActionPreference
        try {
            # taskkill reports an already-exited process on stderr with a
            # non-zero code. That is a normal termination race, not a script
            # error, so consume only this native command's stderr locally.
            $ErrorActionPreference = "Continue"
            & taskkill.exe /PID $process.Id /T /F 2>&1 | Out-Null
        } finally {
            $ErrorActionPreference = $taskkillErrorAction
        }

        if ($process.HasExited) {
            return
        }

        # taskkill may report failure while the process is still transitioning
        # out. Only touch the exact root PID when it is genuinely still alive.
        try {
            $process.Kill()
        } catch {
            if ($process.HasExited) {
                return
            }
            throw
        }
    }
    $process.WaitForExit()
}

function Wait-ForFirstRecord {
    param([Parameter(Mandatory = $true)][Diagnostics.Process]$Process)

    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($Process.HasExited) {
            throw "first eval process exited before the harness could kill it"
        }
        if ((Read-CompleteManifestRecords).Count -ge 1) {
            return
        }
        Start-Sleep -Milliseconds 100
    }
    throw "timed out waiting for the first complete manifest record"
}

$first = $null
$second = $null
$killed = $false
$firstExitCode = $null
$secondExitCode = $null
$firstRecordsBeforeKill = @()

try {
    Set-Location $repoRoot
    & cargo build -p eval --bin ridgecode-eval --locked
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build -p eval --bin ridgecode-eval failed with exit code $LASTEXITCODE"
    }

    $first = Start-EvalProcess -StdoutPath $firstStdoutPath -StderrPath $firstStderrPath
    Wait-ForFirstRecord -Process $first.process
    if ($first.process.HasExited) {
        throw "first eval process exited before kill"
    }
    $firstRecordsBeforeKill = @(Read-CompleteManifestRecords)
    if ($firstRecordsBeforeKill.Count -lt 1 -or $firstRecordsBeforeKill.Count -ge 3) {
        throw "first process kill window had $($firstRecordsBeforeKill.Count) complete records; expected 1 or 2"
    }
    Stop-EvalProcessTree -Run $first
    Save-EvalOutput -Run $first
    $killed = $true
    $firstExitCode = $first.process.ExitCode
    if ($firstExitCode -eq 0) {
        throw "killed first eval process unexpectedly reported zero exit code"
    }

    $second = Start-EvalProcess -StdoutPath $secondStdoutPath -StderrPath $secondStderrPath
    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
    while (-not $second.process.HasExited -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 100
    }
    if (-not $second.process.HasExited) {
        Stop-EvalProcessTree -Run $second
        Save-EvalOutput -Run $second
        throw "second eval process timed out"
    }
    Save-EvalOutput -Run $second
    $secondExitCode = $second.process.ExitCode
    if ($secondExitCode -ne 0) {
        throw "second eval process failed with exit code $secondExitCode"
    }

    $secondJson = [IO.File]::ReadAllText($secondStdoutPath) | ConvertFrom-Json
    if ($null -eq $secondJson.report) {
        throw "second eval process did not emit a report"
    }
    $report = $secondJson.report
    if ([int]$report.resumed -lt 1) {
        throw "resume report did not reuse a completed case"
    }
    if (([int]$report.executed + [int]$report.resumed) -ne [int]$report.total) {
        throw "executed + resumed does not equal total"
    }
    if ([int]$report.total -ne 3 -or [int]$report.passed -ne 3) {
        throw "recovery fixture report is incomplete or unapproved"
    }

    $expectedNames = @("recovery-0", "recovery-1", "recovery-2")
    $actualNames = @($report.results | ForEach-Object { [string]$_.name })
    if ($actualNames.Count -ne $expectedNames.Count) {
        throw "recovery result count is $($actualNames.Count), expected 3"
    }
    for ($index = 0; $index -lt $expectedNames.Count; $index++) {
        if ($actualNames[$index] -ne $expectedNames[$index]) {
            throw "recovery result order changed at index $index"
        }
    }

    $records = @(Read-CompleteManifestRecords)
    $fingerprints = @($records | ForEach-Object { [string]$_.fingerprint })
    $uniqueFingerprints = @($fingerprints | Sort-Object -Unique)
    if ($records.Count -ne 3 -or $uniqueFingerprints.Count -ne $records.Count) {
        throw "manifest has incomplete records or duplicate fingerprints"
    }

    $evidence = [ordered]@{
        schema_version = 1
        run_id = $runId
        first_process_killed = $killed
        first_exit_code = $firstExitCode
        second_exit_code = $secondExitCode
        first_complete_records = $firstRecordsBeforeKill.Count
        manifest_records = $records.Count
        unique_fingerprints = $uniqueFingerprints.Count
        report = [ordered]@{
            resumed = [int]$report.resumed
            executed = [int]$report.executed
            total = [int]$report.total
            passed = [int]$report.passed
            result_names = $actualNames
        }
    }
    Write-Output ($evidence | ConvertTo-Json -Depth 8 -Compress)
} finally {
    foreach ($process in @($first, $second)) {
        if ($null -ne $process) {
            Stop-EvalProcessTree -Run $process
            Save-EvalOutput -Run $process
            $process.process.Dispose()
        }
    }
    foreach ($path in $createdPaths) {
        Remove-CreatedPath -Path $path
    }
}
