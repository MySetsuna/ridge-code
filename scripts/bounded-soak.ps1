[CmdletBinding()]
param(
    [ValidateRange(1, 256)]
    [int]$Iterations = 10,
    [ValidateRange(1, 32)]
    [int]$Concurrency = 3,
    [ValidateRange(1, 300000)]
    [int]$TimeoutMs = 5000,
    [ValidateRange(1000, 3600000)]
    [int]$ProcessTimeoutMs = 30000,
    [ValidateRange(1, 256)]
    [int]$ExpectedCases = 3,
    [ValidateRange(0, 256)]
    [int]$MinimumPassed = 1,
    [string]$Binary = "target/debug/ridgecode-eval.exe",
    [string]$OutputPath = "target/quality/bounded-soak.json"
)

$root = [IO.Path]::GetFullPath((Get-Location).Path)
$binaryPath = [IO.Path]::GetFullPath((Join-Path $root $Binary))
$outputFullPath = [IO.Path]::GetFullPath((Join-Path $root $OutputPath))
$rootPrefix = $root.TrimEnd([char]92) + [char]92

if (-not $binaryPath.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Binary must stay inside the repository: $Binary"
}
if (-not $outputFullPath.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "OutputPath must stay inside the repository: $OutputPath"
}
if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
    throw "Eval binary not found: $binaryPath (run cargo build -p eval --bin ridgecode-eval first)"
}

function Invoke-EvalJson {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $binaryPath
    $start.WorkingDirectory = $root
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.Arguments = "--json --concurrency $Concurrency --timeout-ms $TimeoutMs"

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    if (-not $process.Start()) {
        throw "Could not start eval binary: $binaryPath"
    }
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit($ProcessTimeoutMs)) {
        try { $process.Kill() } catch { }
        $process.WaitForExit()
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        $detail = if ([string]::IsNullOrWhiteSpace($stderr)) { "no stderr" } else { $stderr.Trim() }
        $process.Dispose()
        throw "eval exceeded process timeout ${ProcessTimeoutMs}ms and was terminated: $detail"
    }
    # A parameterless wait flushes redirected asynchronous streams after the
    # timed process wait has observed exit.
    $process.WaitForExit()
    $stdout = $stdoutTask.GetAwaiter().GetResult()
    $stderr = $stderrTask.GetAwaiter().GetResult()

    if ($process.ExitCode -ne 0) {
        $detail = if ([string]::IsNullOrWhiteSpace($stderr)) { "no stderr" } else { $stderr.Trim() }
        $process.Dispose()
        throw "eval exited $($process.ExitCode): $detail"
    }
    try {
        $payload = $stdout | ConvertFrom-Json
        $payload | Add-Member -NotePropertyName process_duration_ms -NotePropertyValue ([long]$watch.ElapsedMilliseconds)
        return $payload
    } catch {
        throw "eval emitted invalid JSON: $($_.Exception.Message)"
    } finally {
        $process.Dispose()
    }
}

$records = [Collections.Generic.List[object]]::new()
for ($iteration = 1; $iteration -le $Iterations; $iteration += 1) {
    $payload = Invoke-EvalJson
    $report = $payload.report
    if ($null -eq $report -or $report.total -ne $ExpectedCases) {
        throw "iteration $iteration returned unexpected case count"
    }
    if ($null -eq $report.results -or @($report.results).Count -ne $ExpectedCases) {
        throw "iteration $iteration returned incomplete results"
    }
    $timedOut = @($report.results | Where-Object { $_.timed_out }).Count
    if ($report.passed -lt $MinimumPassed -or $timedOut -ge $ExpectedCases) {
        throw "iteration $iteration failed bounded health checks (passed=$($report.passed), timed_out=$timedOut)"
    }
    $caseDurations = @($report.results | ForEach-Object {
            if ($null -eq $_.duration_ms -or [long]$_.duration_ms -lt 0) {
                throw "iteration $iteration returned invalid case duration"
            }
            [long]$_.duration_ms
        })
    $timedOutResults = @($report.results | Where-Object { $_.timed_out })
    $records.Add([pscustomobject]@{
            iteration = $iteration
            pass_rate = [double]$payload.pass_rate
            passed = [int]$report.passed
            total = [int]$report.total
            total_tokens = [int]$report.total_tokens
            timed_out_cases = $timedOut
            timeout_observed_steps = [long](($timedOutResults | Measure-Object -Property steps -Sum).Sum)
            timeout_observed_tokens = [long](($timedOutResults | Measure-Object -Property tokens -Sum).Sum)
            total_case_duration_ms = [long](($caseDurations | Measure-Object -Sum).Sum)
            max_case_duration_ms = [long](($caseDurations | Measure-Object -Maximum).Maximum)
            process_duration_ms = [long]$payload.process_duration_ms
        })
}

$result = [pscustomobject]@{
    status = "PASSED"
    binary = $binaryPath.Substring($root.Length).TrimStart([char[]]("\", "/")).Replace("\", "/")
    iterations = $Iterations
    concurrency = $Concurrency
    timeout_ms = $TimeoutMs
    process_timeout_ms = $ProcessTimeoutMs
    expected_cases = $ExpectedCases
    minimum_passed = $MinimumPassed
    records = $records
}
$parent = Split-Path -Parent $outputFullPath
New-Item -ItemType Directory -Force -Path $parent | Out-Null
$json = $result | ConvertTo-Json -Depth 8
Set-Content -LiteralPath $outputFullPath -Value $json -Encoding utf8
$json
