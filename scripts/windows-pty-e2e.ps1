[CmdletBinding()]
param(
    [string]$Binary,
    [int]$Columns = 96,
    [int]$Rows = 24,
    [int]$TimeoutMs = 4000,
    [int]$EnterAfterMs = 500,
    [int]$InterruptAfterMs = 1200,
    [switch]$EscTakeover,
    [switch]$BusyFixture,
    [switch]$StressFixture,
    [switch]$CompletionFixture,
    [switch]$InspectLive,
    [switch]$InspectQueue,
    [switch]$InspectReasoning,
    [switch]$InspectAnswer,
    [switch]$InspectHold,
    [switch]$ResizeProbe,
    [switch]$KeepDiagnostics
)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($Binary)) {
    $Binary = Join-Path $PSScriptRoot '..\target\debug\ridgecode.exe'
}
$binaryPath = [System.IO.Path]::GetFullPath($Binary)
if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
    throw "ridgecode binary not found: $binaryPath"
}
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'windows-pty-e2e.ps1 requires Windows ConPTY'
}
if ($BusyFixture -and $CompletionFixture) {
    throw '-BusyFixture and -CompletionFixture are mutually exclusive'
}
if ($StressFixture -and ($BusyFixture -or $CompletionFixture)) {
    throw '-StressFixture is mutually exclusive with -BusyFixture and -CompletionFixture'
}
if ($EscTakeover -and -not $BusyFixture) {
    throw '-EscTakeover requires -BusyFixture'
}
if ($StressFixture) {
    # The stress fixture is a completion path with a live reasoning phase;
    # repeated resize is part of its acceptance contract.
    $ResizeProbe = $true
}
$stressFixtureRequested = [bool]$StressFixture
$completionMode = [bool]($CompletionFixture -or $stressFixtureRequested)
if ($InspectLive -and -not $BusyFixture) {
    throw '-InspectLive requires -BusyFixture so a live block exists to inspect'
}
if ($InspectAnswer -and -not $completionMode) {
    throw '-InspectAnswer requires -CompletionFixture or -StressFixture so a live answer exists'
}
if ($InspectQueue -and (-not $BusyFixture -or -not $InspectLive)) {
    throw '-InspectQueue requires -BusyFixture -InspectLive'
}
if (($InspectReasoning -or $InspectHold) -and -not $BusyFixture) {
    throw '-InspectReasoning/-InspectHold require -BusyFixture'
}

if (-not ('RidgeCode.ConPtyNative' -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

namespace RidgeCode {
    public sealed class ConPtyNative : IDisposable {
        private const uint EXTENDED_STARTUPINFO_PRESENT = 0x00080000;
        private const uint CREATE_UNICODE_ENVIRONMENT = 0x00000400;
        private const int STARTF_USESTDHANDLES = 0x00000100;
        private const uint HANDLE_FLAG_INHERIT = 0x00000001;
        private const uint LMEM_FIXED = 0x0000;
        private const long PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE = 0x00020016;
        private const uint WAIT_OBJECT_0 = 0x00000000;

        [StructLayout(LayoutKind.Sequential)]
        private struct Coord { public short X; public short Y; }

        [StructLayout(LayoutKind.Sequential)]
        private struct SecurityAttributes {
            public int Length;
            public IntPtr SecurityDescriptor;
            [MarshalAs(UnmanagedType.Bool)] public bool InheritHandle;
        }

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        private struct StartupInfo {
            public int Cb;
            public string Reserved;
            public string Desktop;
            public string Title;
            public int X;
            public int Y;
            public int XSize;
            public int YSize;
            public int XCountChars;
            public int YCountChars;
            public int FillAttribute;
            public int Flags;
            public short ShowWindow;
            public short Reserved2;
            public IntPtr Reserved2Ptr;
            public IntPtr StdInput;
            public IntPtr StdOutput;
            public IntPtr StdError;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct StartupInfoEx {
            public StartupInfo StartupInfo;
            public IntPtr AttributeList;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct ProcessInformation {
            public IntPtr Process;
            public IntPtr Thread;
            public int ProcessId;
            public int ThreadId;
        }

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool CreatePipe(
            out IntPtr readHandle,
            out IntPtr writeHandle,
            ref SecurityAttributes attributes,
            int size);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool SetHandleInformation(IntPtr handle, uint mask, uint flags);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern int CreatePseudoConsole(
            Coord size,
            IntPtr inputRead,
            IntPtr outputWrite,
            uint flags,
            out IntPtr pseudoConsole);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern int ClosePseudoConsole(IntPtr pseudoConsole);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern int ResizePseudoConsole(IntPtr pseudoConsole, Coord size);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool InitializeProcThreadAttributeList(
            IntPtr attributeList,
            int attributeCount,
            int flags,
            ref IntPtr size);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool UpdateProcThreadAttribute(
            IntPtr attributeList,
            uint flags,
            IntPtr attribute,
            IntPtr value,
            IntPtr size,
            IntPtr previousValue,
            IntPtr returnSize);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern void DeleteProcThreadAttributeList(IntPtr attributeList);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr LocalAlloc(uint flags, UIntPtr bytes);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr LocalFree(IntPtr memory);

        [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        private static extern bool CreateProcess(
            string applicationName,
            StringBuilder commandLine,
            IntPtr processAttributes,
            IntPtr threadAttributes,
            bool inheritHandles,
            uint creationFlags,
            IntPtr environment,
            string currentDirectory,
            ref StartupInfoEx startupInfo,
            out ProcessInformation processInformation);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool CloseHandle(IntPtr handle);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool WriteFile(
            IntPtr file,
            byte[] buffer,
            int bytesToWrite,
            out int bytesWritten,
            IntPtr overlapped);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool PeekNamedPipe(
            IntPtr pipe,
            IntPtr buffer,
            int bufferSize,
            out int bytesRead,
            out int bytesAvailable,
            out int bytesLeftThisMessage);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool ReadFile(
            IntPtr file,
            byte[] buffer,
            int bytesToRead,
            out int bytesRead,
            IntPtr overlapped);

        private IntPtr _inputWrite;
        private IntPtr _outputRead;
        private IntPtr _pseudoConsole;
        private IntPtr _process;
        private IntPtr _thread;

        public int ProcessId { get; private set; }
        public int BytesWritten { get; private set; }
        public int BytesRead { get; private set; }

        private ConPtyNative() { }

        public static ConPtyNative Start(string applicationName, string currentDirectory, short columns, short rows) {
            var result = new ConPtyNative();
            IntPtr inputRead = IntPtr.Zero;
            IntPtr outputWrite = IntPtr.Zero;
            IntPtr attributeList = IntPtr.Zero;
            SecurityAttributes pipeAttributes = new SecurityAttributes {
                Length = Marshal.SizeOf(typeof(SecurityAttributes)),
                InheritHandle = true,
            };
            try {
                Ensure(CreatePipe(out inputRead, out result._inputWrite, ref pipeAttributes, 0), "CreatePipe(input)");
                Ensure(CreatePipe(out result._outputRead, out outputWrite, ref pipeAttributes, 0), "CreatePipe(output)");
                Ensure(SetHandleInformation(result._inputWrite, HANDLE_FLAG_INHERIT, 0), "SetHandleInformation(input)");
                Ensure(SetHandleInformation(result._outputRead, HANDLE_FLAG_INHERIT, 0), "SetHandleInformation(output)");

                var size = new Coord { X = columns, Y = rows };
                Ensure(CreatePseudoConsole(size, inputRead, outputWrite, 0, out result._pseudoConsole) == 0, "CreatePseudoConsole");

                IntPtr attributeSize = IntPtr.Zero;
                InitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref attributeSize);
                attributeList = Marshal.AllocHGlobal(attributeSize);
                Ensure(InitializeProcThreadAttributeList(attributeList, 1, 0, ref attributeSize), "InitializeProcThreadAttributeList");
                Ensure(UpdateProcThreadAttribute(
                    attributeList,
                    0,
                    (IntPtr)PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                    result._pseudoConsole,
                    (IntPtr)IntPtr.Size,
                    IntPtr.Zero,
                    IntPtr.Zero), "UpdateProcThreadAttribute");

                var startup = new StartupInfoEx();
                startup.StartupInfo.Cb = Marshal.SizeOf(typeof(StartupInfoEx));
                startup.StartupInfo.Flags = STARTF_USESTDHANDLES;
                startup.StartupInfo.StdInput = inputRead;
                startup.StartupInfo.StdOutput = outputWrite;
                startup.StartupInfo.StdError = outputWrite;
                startup.AttributeList = attributeList;
                var commandLine = new StringBuilder("\"" + applicationName + "\"");
                ProcessInformation process;
                Ensure(CreateProcess(
                    applicationName,
                    commandLine,
                    IntPtr.Zero,
                    IntPtr.Zero,
                    true,
                    EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                    IntPtr.Zero,
                    currentDirectory,
                    ref startup,
                    out process), "CreateProcess");
                result._process = process.Process;
                result._thread = process.Thread;
                result.ProcessId = process.ProcessId;
                CloseHandle(inputRead); inputRead = IntPtr.Zero;
                CloseHandle(outputWrite); outputWrite = IntPtr.Zero;
                return result;
            } catch {
                result.Dispose();
                if (inputRead != IntPtr.Zero) CloseHandle(inputRead);
                if (outputWrite != IntPtr.Zero) CloseHandle(outputWrite);
                throw;
            } finally {
                if (attributeList != IntPtr.Zero) {
                    DeleteProcThreadAttributeList(attributeList);
                    Marshal.FreeHGlobal(attributeList);
                }
            }
        }

        public bool HasExited {
            get { return _process == IntPtr.Zero || WaitForSingleObject(_process, 0) == WAIT_OBJECT_0; }
        }

        public void Send(byte[] bytes) {
            if (bytes == null || bytes.Length == 0) return;
            int written;
            Ensure(WriteFile(_inputWrite, bytes, bytes.Length, out written, IntPtr.Zero), "WriteFile");
            BytesWritten += written;
            if (written != bytes.Length) throw new IOException("ConPTY input write was partial");
        }

        public void Resize(short columns, short rows) {
            Ensure(ResizePseudoConsole(
                _pseudoConsole,
                new Coord { X = columns, Y = rows }) == 0,
                "ResizePseudoConsole");
        }

        public byte[] ReadAvailable() {
            int ignored;
            int available;
            int left;
            if (!PeekNamedPipe(_outputRead, IntPtr.Zero, 0, out ignored, out available, out left)) {
                return new byte[0];
            }
            if (available <= 0) return new byte[0];
            var buffer = new byte[Math.Min(available, 16384)];
            int read;
            Ensure(ReadFile(_outputRead, buffer, buffer.Length, out read, IntPtr.Zero), "ReadFile");
            if (read == buffer.Length) {
                BytesRead += read;
                return buffer;
            }
            var trimmed = new byte[read];
            Array.Copy(buffer, trimmed, read);
            BytesRead += read;
            return trimmed;
        }

        public void Dispose() {
            if (_thread != IntPtr.Zero) { CloseHandle(_thread); _thread = IntPtr.Zero; }
            if (_process != IntPtr.Zero) { CloseHandle(_process); _process = IntPtr.Zero; }
            if (_inputWrite != IntPtr.Zero) { CloseHandle(_inputWrite); _inputWrite = IntPtr.Zero; }
            if (_outputRead != IntPtr.Zero) { CloseHandle(_outputRead); _outputRead = IntPtr.Zero; }
            if (_pseudoConsole != IntPtr.Zero) { ClosePseudoConsole(_pseudoConsole); _pseudoConsole = IntPtr.Zero; }
        }

        private static void Ensure(bool ok, string operation) {
            if (!ok) throw new Win32Exception(Marshal.GetLastWin32Error(), operation);
        }

        private static void Ensure(bool ok, string operation, int error) {
            if (!ok) throw new Win32Exception(error, operation);
        }
    }
}
"@
}

$session = $null
$text = New-Object System.Text.StringBuilder
$deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
$sentHelp = $false
$completionTaskSent = $false
$completionObserved = $false
$snapshotCompletionRaw = ''
$snapshotCompletionJson = $null
$sentInterrupt = $false
$sentFront = $false
$sentInspect = $false
$sentInspectSpace = $false
$sentInspectEnd = $false
$sentInspectDelete = $false
$sentQueueSwitch = $false
$sentInspectReturn = $false
$sentReasoning = $false
$reasoningObserved = $false
$sentAnswerInspect = $false
$answerInspectObserved = $false
$sentHold = $false
$holdObserved = $false
$sentFollow = $false
$followObserved = $false
$sentResize = $false
$resizeObserved = $false
$resizeColumns = if ($Columns -ge 80) { 40 } else { 96 }
$resizeRows = if ($Rows -ge 16) { 12 } else { 24 }
$resizeTargets = if ($StressFixture) {
    @(
        [pscustomobject]@{ at = 1100; columns = $resizeColumns; rows = $resizeRows },
        [pscustomobject]@{ at = 1700; columns = $Columns; rows = $Rows },
        [pscustomobject]@{ at = 2300; columns = $resizeColumns; rows = $resizeRows }
    )
} else {
    @([pscustomobject]@{ at = 1100; columns = $resizeColumns; rows = $resizeRows })
}
$resizeTargets = @($resizeTargets)
$resizeTargetIndex = 0
$resizeObservedCount = 0
$resizeLastSentAt = -1
$inlineHeightCap = 14
$resizeFrameRows = [Math]::Min($resizeRows, $inlineHeightCap)
$snapshotResizeRaw = ''
$snapshotResizeJson = $null
$followAfterMs = if ($InspectLive) { 1600 } else { 1900 }
$frontFallbackSent = $false
$inspectObserved = $false
$inspectExpandedObserved = $false
$inspectQueueRemovedObserved = $false
$attentionQueueObserved = $false
$attentionLiveObserved = $false
$aliveAfterEnter = $false
$sentEsc = $false
$snapshotMidRaw = ''
$snapshotMidJson = $null
$snapshotInspectRaw = ''
$snapshotInspectJson = $null
$snapshotAnswerInspectRaw = ''
$snapshotAnswerInspectJson = $null
$effectiveInterruptAfterMs = if ($BusyFixture) {
    [Math]::Max($InterruptAfterMs, $(if ($InspectQueue) { 3600 } elseif ($InspectLive) { 3200 } elseif ($InspectReasoning -or $InspectHold) { 2800 } else { 2200 }))
} elseif ($CompletionFixture) {
    [Math]::Max($InterruptAfterMs, $(if ($InspectAnswer) { 3400 } else { 2400 }))
} elseif ($stressFixtureRequested) {
    [Math]::Max($InterruptAfterMs, $(if ($InspectAnswer) { 5000 } else { 3400 }))
} else {
    $InterruptAfterMs
}
$rawOutput = New-Object 'System.Collections.Generic.List[byte]'
$previousConfig = [Environment]::GetEnvironmentVariable('RIDGE_CONFIG', 'Process')
$isolatedConfig = Join-Path ([IO.Path]::GetTempPath()) "ridgecode-pty-$PID.json"
$isolatedAuth = Join-Path ([IO.Path]::GetTempPath()) "ridgecode-pty-$PID-auth.json"
$isolatedOauth = Join-Path ([IO.Path]::GetTempPath()) "ridgecode-pty-$PID-oauth.json"
$isolatedHome = Join-Path ([IO.Path]::GetTempPath()) "ridgecode-pty-$PID-home"
$isolatedSnapshot = Join-Path $isolatedHome '.ridge\frame.json'
$isolatedTrace = Join-Path $isolatedHome '.ridge\tui-trace.log'
$isolatedVariables = @(
    'RIDGE_PROVIDER', 'RIDGE_MODEL', 'RIDGE_BASE_URL', 'RIDGE_API_KEY',
    'RIDGE_READ_ONLY', 'RIDGE_SKIP_PERMISSIONS', 'RIDGE_MCP', 'RIDGE_AUTH', 'RIDGE_OAUTH',
    'RIDGE_KEYLOG', 'RIDGE_TUI_SNAPSHOT', 'RIDGE_FORCE_TUI', 'RIDGE_TUI_FIXTURE', 'RIDGE_TUI_TRACE', 'RIDGE_TUI_KITTY', 'RIDGE_TUI_INSPECT_ANSWER'
)
$previousVariables = @{}
foreach ($name in $isolatedVariables) {
    $previousVariables[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
$binaryDirectory = Split-Path -Parent $binaryPath
$previousUserProfile = [Environment]::GetEnvironmentVariable('USERPROFILE', 'Process')

try {
    New-Item -ItemType Directory -Path (Join-Path $isolatedHome '.ridge') -Force | Out-Null
    $env:USERPROFILE = $isolatedHome
    $env:RIDGE_CONFIG = $isolatedConfig
    foreach ($name in $isolatedVariables) { Remove-Item "Env:$name" -ErrorAction SilentlyContinue }
    $env:RIDGE_AUTH = $isolatedAuth
    $env:RIDGE_OAUTH = $isolatedOauth
    $env:RIDGE_PROVIDER = 'openai'
    $env:RIDGE_MODEL = 'pty-test-model'
    $env:RIDGE_BASE_URL = 'http://127.0.0.1:9/v1'
    $env:RIDGE_API_KEY = 'pty-test-key'
    $env:RIDGE_KEYLOG = '1'
    $env:RIDGE_TUI_SNAPSHOT = $isolatedSnapshot
    $env:RIDGE_FORCE_TUI = '1'
    $env:RIDGE_TUI_TRACE = $isolatedTrace
    if ($InspectAnswer) {
        $env:RIDGE_TUI_INSPECT_ANSWER = '1'
    }
    if ($StressFixture) {
        $env:RIDGE_TUI_FIXTURE = 'stress'
    } elseif ($BusyFixture) {
        $env:RIDGE_TUI_FIXTURE = 'busy'
        $env:RIDGE_TUI_KITTY = '1'
    } elseif ($CompletionFixture) {
        $env:RIDGE_TUI_FIXTURE = 'complete'
    }
    [IO.File]::WriteAllText(
        $isolatedConfig,
        '{"provider":"openai","model":"pty-test-model","base_url":"http://127.0.0.1:9/v1"}'
    )
    $session = [RidgeCode.ConPtyNative]::Start($binaryPath, $binaryDirectory, [int16]$Columns, [int16]$Rows)
    while ([DateTime]::UtcNow -lt $deadline -and -not $session.HasExited) {
        $bytes = $session.ReadAvailable()
        if ($bytes.Length -gt 0) {
            $rawOutput.AddRange($bytes)
            [void]$text.Append([Text.Encoding]::UTF8.GetString($bytes))
        }
        $elapsed = ($TimeoutMs - ($deadline - [DateTime]::UtcNow).TotalMilliseconds)
        if (-not $sentHelp -and $elapsed -ge $EnterAfterMs) {
            $session.Send([Text.Encoding]::UTF8.GetBytes('/help'))
            $session.Send([byte[]](0x0d))
            $sentHelp = $true
        }
        if ($completionMode -and $sentHelp -and -not $completionTaskSent -and $elapsed -ge 850) {
            $session.Send([Text.Encoding]::UTF8.GetBytes('completion fixture task'))
            $session.Send([byte[]](0x0d))
            $completionTaskSent = $true
        }
        if ($InspectAnswer -and $completionTaskSent -and -not $sentAnswerInspect -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.state -and $parsed.state.busy -and
                    $parsed.state.live_blocks -gt 0 -and $parsed.state.live_trace -match 'ANS') {
                    # Ctrl+A is the contextual live-answer audit shortcut; send
                    # it only after the snapshot proves the Answer block exists.
                    $session.Send([byte[]](0x01))
                    $sentAnswerInspect = $true
                }
            } catch {
                # Retry after the frame writer leaves its transient state.
            }
        }
        if ($InspectAnswer -and $sentAnswerInspect -and -not $answerInspectObserved -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.state -and $parsed.state.busy -and
                    $parsed.state.live_view -eq 'hold' -and $parsed.state.live_focus -match '^answer:') {
                    $snapshotAnswerInspectRaw = $candidate
                    $snapshotAnswerInspectJson = $parsed
                    $answerInspectObserved = $true
                }
            } catch {
                # Retry after the frame writer leaves its transient state.
            }
        }
        if ($completionMode -and $completionTaskSent -and -not $completionObserved -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.state -and
                    -not $parsed.state.busy -and
                    $parsed.state.reasoning_history -ge 1 -and
                    $parsed.state.answer_history -ge 1) {
                    $snapshotCompletionRaw = $candidate
                    $snapshotCompletionJson = $parsed
                    $completionObserved = $true
                }
            } catch {
                # Retry after the frame write completes.
            }
        }
        if ($BusyFixture -and -not $sentFront -and $elapsed -ge 850) {
            $session.Send([Text.Encoding]::UTF8.GetBytes('/front'))
            # BusyFixture opts into Kitty disambiguation.  Exercise the real
            # physical Ctrl+Enter spelling instead of relying on a platform's
            # CR/LF-to-KeyCode fallback; input.rs still normalizes that fallback
            # for legacy Windows terminals.
            $session.Send([byte[]](0x1b, 0x5b, 0x31, 0x33, 0x3b, 0x35, 0x75))
            $sentFront = $true
        }
        if ($BusyFixture -and $InspectReasoning -and $sentFront -and -not $sentReasoning -and $elapsed -ge 1400) {
            # Ctrl+R is the physical control byte used by Windows ConPTY;
            # the application maps it to live reasoning/history inspection.
            $session.Send([byte[]](0x12))
            $sentReasoning = $true
        }
        if ($InspectReasoning -and $sentReasoning -and -not $reasoningObserved -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.state -and $parsed.state.reasoning_expanded) {
                    $reasoningObserved = $true
                }
            } catch {
                # Retry after the frame write completes.
            }
        }
        if ($BusyFixture -and $InspectHold -and $sentFront -and -not $sentHold -and $elapsed -ge 1500) {
            # Ctrl+Space is NUL in the raw Windows terminal stream.
            $session.Send([byte[]](0x00))
            $sentHold = $true
        }
        if ($InspectHold -and $sentHold -and -not $holdObserved -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.state -and $parsed.state.live_view -eq 'hold') {
                    $holdObserved = $true
                }
            } catch {
                # Retry after the frame write completes.
            }
        }
        if ($InspectHold -and $holdObserved -and -not $sentFollow -and $elapsed -ge $followAfterMs) {
            $session.Send([byte[]](0x00))
            $sentFollow = $true
        }
        if ($InspectHold -and $sentFollow -and -not $followObserved -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.state -and $parsed.state.live_view -eq 'follow') {
                    $followObserved = $true
                }
            } catch {
                # Retry after the frame write completes.
            }
        }
        if ($ResizeProbe -and $resizeTargetIndex -lt $resizeTargets.Count -and $elapsed -ge $resizeTargets[$resizeTargetIndex].at) {
            $target = $resizeTargets[$resizeTargetIndex]
            $session.Resize([int16]$target.columns, [int16]$target.rows)
            $sentResize = $true
            $resizeTargetIndex++
            $resizeLastSentAt = $elapsed
        }
        if ($ResizeProbe -and $resizeTargetIndex -gt $resizeObservedCount -and ($elapsed - $resizeLastSentAt) -ge 80 -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                $target = $resizeTargets[$resizeObservedCount]
                if ($null -ne $parsed.rect -and
                    $parsed.rect.width -eq $target.columns -and
                    $parsed.rect.height -eq [Math]::Min($target.rows, $inlineHeightCap)) {
                    $snapshotResizeRaw = $candidate
                    $snapshotResizeJson = $parsed
                    $resizeObservedCount++
                    $resizeObserved = $resizeObservedCount -eq $resizeTargets.Count
                }
            } catch {
                # Retry after the frame write completes.
            }
        }
        if ($BusyFixture -and $InspectLive -and $sentFront -and -not $sentInspect -and $elapsed -ge 1700) {
            # Alt+I is the byte-safe spelling of the live inspector.  Ctrl+I
            # is accepted interactively too, but raw 0x09 can be indistinguishable
            # from Tab on hosts that collapse Ctrl+I to the completion key.
            $session.Send([byte[]](0x1b, 0x69))
            $sentInspect = $true
        }
        if ($sentInspect -and -not $inspectObserved -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.panel -and $parsed.panel.kind -match '^(Live|Audit)$') {
                    $snapshotInspectRaw = $candidate
                    $snapshotInspectJson = $parsed
                    $inspectObserved = $true
                }
            } catch {
                # The writer may be between truncate/write; retry next loop.
            }
        }
        if ($inspectObserved -and -not $sentInspectSpace -and $elapsed -ge 2100) {
            # LiveHistory maps an unmodified Space to expand the selected block.
            $session.Send([byte[]](0x20))
            $sentInspectSpace = $true
        }
        if ($sentInspectSpace -and -not $inspectExpandedObserved -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.panel -and $parsed.panel.kind -match '^(Live|Audit)$' -and $parsed.panel.detail_open) {
                    $inspectExpandedObserved = $true
                    $snapshotInspectRaw = $candidate
                    $snapshotInspectJson = $parsed
                }
            } catch {
                # Retry after the frame write completes.
            }
        }
        if ($InspectQueue -and $inspectExpandedObserved -and -not $sentInspectEnd -and $elapsed -ge 2250) {
            # End selects the last mixed row, which is the last pending message
            # in the Inspector's actionable FIFO rail.
            $session.Send([byte[]](0x1b, 0x5b, 0x46))
            $sentInspectEnd = $true
        }
        if ($InspectQueue -and $sentInspectEnd -and -not $sentInspectDelete -and $elapsed -ge 2350) {
            # CSI 3~ is Delete in the Windows/ConPTY crossterm path.
            $session.Send([byte[]](0x1b, 0x5b, 0x33, 0x7e))
            $sentInspectDelete = $true
        }
        if ($InspectQueue -and $sentInspectDelete -and -not $inspectQueueRemovedObserved -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.state -and $parsed.state.queued -eq 1 -and $null -ne $parsed.panel -and $parsed.panel.kind -match '^(Live|Audit)$') {
                    $inspectQueueRemovedObserved = $true
                }
            } catch {
                # Retry after the frame write completes.
            }
        }
        if ($InspectQueue -and $inspectQueueRemovedObserved -and -not $sentQueueSwitch -and $elapsed -ge 2550) {
            # Ctrl+Q moves directly from the read-only Inspector to full FIFO.
            $session.Send([byte[]](0x11))
            $sentQueueSwitch = $true
        }
        if ($InspectQueue -and $sentQueueSwitch -and -not $attentionQueueObserved -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.panel -and $parsed.panel.kind -eq 'Queue') {
                    $attentionQueueObserved = $true
                }
            } catch {
                # Retry after the frame write completes.
            }
        }
        if ($InspectQueue -and $attentionQueueObserved -and -not $sentInspectReturn -and $elapsed -ge 2750) {
            # Alt+I is the byte-safe Inspector toggle from the queue panel.
            $session.Send([byte[]](0x1b, 0x69))
            $sentInspectReturn = $true
        }
        if ($InspectQueue -and $sentInspectReturn -and -not $attentionLiveObserved -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.panel -and $parsed.panel.kind -match '^(Live|Audit)$') {
                    $attentionLiveObserved = $true
                }
            } catch {
                # Retry after the frame write completes.
            }
        }
        if ($BusyFixture -and $sentFront -and $null -eq $snapshotMidJson -and $elapsed -ge 1000) {
            if (Test-Path -LiteralPath $isolatedSnapshot) {
                try {
                    $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                    $parsed = $candidate | ConvertFrom-Json
                    $queue = if ($null -ne $parsed.state) { @($parsed.state.queue) } else { @() }
                    if ($null -ne $parsed.state -and $parsed.state.busy -and $parsed.state.queued -ge 2 -and $queue[0] -eq '/front') {
                        $snapshotMidRaw = $candidate
                        $snapshotMidJson = $parsed
                    }
                } catch {
                    # The writer may be between truncate/write; retry next loop.
                }
            }
        }
        if ($BusyFixture -and $sentFront -and -not $frontFallbackSent -and $elapsed -ge 1200) {
            # Some Windows ConPTY/INPUT_RECORD hosts consume CSI-u without
            # surfacing a crossterm KeyEvent.  Keep the attempt observable, then
            # fall back to the physical CR/LF spelling that those hosts expose
            # as Ctrl+Enter.  Do not send both when the queue frame proves CSI-u.
            $frontSeen = $false
            if (Test-Path -LiteralPath $isolatedSnapshot) {
                try {
                    $probe = [IO.File]::ReadAllText($isolatedSnapshot) | ConvertFrom-Json
                    $probeQueue = if ($null -ne $probe.state) { @($probe.state.queue) } else { @() }
                    $frontSeen = $null -ne $probe.state -and $probe.state.busy -and $probe.state.queued -ge 2 -and $probeQueue[0] -eq '/front'
                } catch {
                    # Retry once more after the writer leaves its transient state.
                }
            }
            if (-not $frontSeen) {
                $session.Send([byte[]](0x0a))
                $frontFallbackSent = $true
            }
        }
        if ($sentHelp -and -not $sentInterrupt -and $elapsed -ge $effectiveInterruptAfterMs) {
            $aliveAfterEnter = -not $session.HasExited
            if (-not $aliveAfterEnter) { throw 'ridgecode exited before the takeover probe' }
            if ($EscTakeover) {
                # Escape is the uncovered busy-surface takeover path; retain
                # the existing two-Ctrl-C termination probe afterward.
                $session.Send([byte[]](0x1b))
                $sentEsc = $true
                Start-Sleep -Milliseconds 120
            }
            $session.Send([byte[]](0x03))
            Start-Sleep -Milliseconds 80
            $session.Send([byte[]](0x03))
            $sentInterrupt = $true
        }
        Start-Sleep -Milliseconds 10
    }
    $bytes = $session.ReadAvailable()
    if ($bytes.Length -gt 0) {
        $rawOutput.AddRange($bytes)
        [void]$text.Append([Text.Encoding]::UTF8.GetString($bytes))
    }
    $keylogPath = Join-Path $isolatedHome '.ridge\keylog.txt'
    $keylog = if (Test-Path -LiteralPath $keylogPath) {
        [IO.File]::ReadAllText($keylogPath)
    } else {
        ''
    }
    $keylogEvents = @($keylog -split "`r?`n" | Where-Object { $_.Trim().Length -gt 0 })
    $keylogHasEnter = $keylog -match 'Enter'
    $keylogHasCtrlC = $keylog -match 'CONTROL'
    $crosstermEventsObserved = $keylogHasEnter -and $keylogHasCtrlC
    $snapshotRaw = if (Test-Path -LiteralPath $isolatedSnapshot) {
        [IO.File]::ReadAllText($isolatedSnapshot)
    } else {
        ''
    }
    $snapshotJson = $null
    if ($snapshotRaw.Length -gt 0) {
        try {
            $snapshotJson = $snapshotRaw | ConvertFrom-Json
        } catch {
            Write-Warning "TUI frame snapshot is not valid JSON: $($_.Exception.Message)"
        }
    }
    $snapshotRows = if ($null -ne $snapshotJson -and $null -ne $snapshotJson.rows) {
        @($snapshotJson.rows) -join "`n"
    } else {
        ''
    }
    $snapshotRenderUs = if ($null -ne $snapshotJson) {
        $snapshotJson.render_us
    } else {
        $null
    }
    $snapshotState = if ($null -ne $snapshotJson) {
        $snapshotJson.state
    } else {
        $null
    }
    if ($completionMode -and -not $completionObserved -and $null -ne $snapshotState -and
        -not $snapshotState.busy -and $snapshotState.reasoning_history -ge 1 -and
        $snapshotState.answer_history -ge 1) {
        $snapshotCompletionRaw = $snapshotRaw
        $snapshotCompletionJson = $snapshotJson
        $completionObserved = $true
    }
    $snapshotMidState = if ($null -ne $snapshotMidJson) {
        $snapshotMidJson.state
    } else {
        $null
    }
    $busyFixtureFrontObserved = if (-not $BusyFixture) {
        $true
    } elseif ($null -ne $snapshotMidState -and $snapshotMidState.queued -ge 2) {
        @($snapshotMidState.queue)[0] -eq '/front'
    } else {
        $false
    }
    $queueAffordanceObserved = $snapshotRows -match '(?i)next'
    $queueEvidenceSatisfied = -not $BusyFixture -or $queueAffordanceObserved
    $inspectEvidenceSatisfied = -not $InspectLive -or ($inspectObserved -and $inspectExpandedObserved)
    $inspectQueueEvidenceSatisfied = -not $InspectQueue -or ($inspectQueueRemovedObserved -and $attentionQueueObserved -and $attentionLiveObserved)
    $reasoningEvidenceSatisfied = -not $InspectReasoning -or $reasoningObserved
    $answerInspectEvidenceSatisfied = -not $InspectAnswer -or $answerInspectObserved
    $holdEvidenceSatisfied = -not $InspectHold -or ($holdObserved -and $followObserved)
    $resizeEvidenceSatisfied = -not $ResizeProbe -or $resizeObserved
    $trace = if (Test-Path -LiteralPath $isolatedTrace) {
        [IO.File]::ReadAllText($isolatedTrace)
    } else {
        ''
    }
    # ConPTY captures cursor-addressed cells, so terminal updates may split a
    # word with CSI moves.  Strip ANSI first, then use whitespace-tolerant
    # probes below; this checks the rendered byte stream rather than a single
    # chunk boundary.
    $ansiPattern = '\x1B(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1B\\))'
    $plain = [regex]::Replace($text.ToString(), $ansiPattern, '')
    $rawOutputPath = Join-Path $isolatedHome '.ridge\pty-output.bin'
    if ($KeepDiagnostics) {
        [IO.File]::WriteAllBytes($rawOutputPath, $rawOutput.ToArray())
    }
    $completionReasoningObserved = if ($stressFixtureRequested) {
        $plain -match 'STRESS_REASONING_END'
    } else {
        $plain -match 'fixture\s*reasoning\s*:\s*completed\s*path\s*remains\s*inspectable'
    }
    $completionAnswerObserved = if ($stressFixtureRequested) {
        $plain -match 'STRESS_ANSWER_BEGIN'
    } else {
        $plain -match 'fixture\s*answer\s*:\s*final\s*response\s*reached\s*scrollback'
    }
    $completionTextObserved = -not $completionMode -or ($completionReasoningObserved -and $completionAnswerObserved)
    $completionEvidenceSatisfied = -not $completionMode -or ($completionTaskSent -and $completionObserved -and $completionTextObserved)
    $takeoverEvidenceSatisfied = -not $EscTakeover -or $sentEsc
    if ($session.BytesRead -eq 0) {
        throw 'ConPTY produced no output bytes'
    }
    if (-not $sentHelp -or -not $sentInterrupt -or -not $aliveAfterEnter) {
        throw 'ConPTY probe did not complete the Enter-then-interrupt sequence'
    }
    if (-not $session.HasExited) {
        throw "ridgecode did not exit after two Ctrl-C bytes within ${TimeoutMs}ms"
    }
    if (-not $crosstermEventsObserved) {
        Write-Warning 'ConPTY byte pipe did not surface crossterm Windows INPUT_RECORD events; raw pipe boundary only.'
    }
    if ($snapshotRaw.Length -eq 0) {
        Write-Warning 'ConPTY probe did not produce an application frame snapshot; readable TUI frame remains unverified.'
    }
    $outputPrefixHex = (($rawOutput | Select-Object -First 64 | ForEach-Object { '{0:X2}' -f $_ }) -join '')
    [pscustomobject]@{
        status = if ($crosstermEventsObserved -and $busyFixtureFrontObserved -and $queueEvidenceSatisfied -and $inspectEvidenceSatisfied -and $inspectQueueEvidenceSatisfied -and $reasoningEvidenceSatisfied -and $answerInspectEvidenceSatisfied -and $holdEvidenceSatisfied -and $resizeEvidenceSatisfied -and $completionEvidenceSatisfied -and $takeoverEvidenceSatisfied) { 'passed' } else { 'partial' }
        binary = $binaryPath
        pid = $session.ProcessId
        columns = $Columns
        rows = $Rows
        input_bytes = $session.BytesWritten
        output_bytes = $session.BytesRead
        output_prefix_hex = $outputPrefixHex
        output_text_preview = if ($plain.Length -gt 1000) { $plain.Substring(0, 1000) } else { $plain }
        output_text_tail = if ($plain.Length -gt 1000) { $plain.Substring($plain.Length - 1000) } else { $plain }
        raw_output_path = if ($KeepDiagnostics) { $rawOutputPath } else { $null }
        output_has_ridge_marker = ($plain -match 'RIDGE|RidgeCode|ready')
        output_has_completion_reasoning = $completionReasoningObserved
        output_has_completion_answer = $completionAnswerObserved
        snapshot_bytes = [Text.Encoding]::UTF8.GetByteCount($snapshotRaw)
        snapshot_render_us = $snapshotRenderUs
        snapshot_json_valid = ($null -ne $snapshotJson)
        snapshot_mid_bytes = [Text.Encoding]::UTF8.GetByteCount($snapshotMidRaw)
        snapshot_mid_json_valid = ($null -ne $snapshotMidJson)
        snapshot_completion_bytes = [Text.Encoding]::UTF8.GetByteCount($snapshotCompletionRaw)
        snapshot_completion_json_valid = ($null -ne $snapshotCompletionJson)
        snapshot_inspector_bytes = [Text.Encoding]::UTF8.GetByteCount($snapshotInspectRaw)
        snapshot_inspector_json_valid = ($null -ne $snapshotInspectJson)
        snapshot_inspector_render_us = if ($null -ne $snapshotInspectJson) { $snapshotInspectJson.render_us } else { $null }
        snapshot_inspector_live_blocks = if ($null -ne $snapshotInspectJson -and $null -ne $snapshotInspectJson.state) { $snapshotInspectJson.state.live_blocks } else { $null }
        snapshot_inspector_live_focus = if ($null -ne $snapshotInspectJson -and $null -ne $snapshotInspectJson.state) { $snapshotInspectJson.state.live_focus } else { $null }
        snapshot_inspector_panel_kind = if ($null -ne $snapshotInspectJson -and $null -ne $snapshotInspectJson.panel) { $snapshotInspectJson.panel.kind } else { $null }
        snapshot_inspector_detail_open = if ($null -ne $snapshotInspectJson -and $null -ne $snapshotInspectJson.panel) { $snapshotInspectJson.panel.detail_open } else { $null }
        snapshot_answer_inspect_bytes = [Text.Encoding]::UTF8.GetByteCount($snapshotAnswerInspectRaw)
        snapshot_answer_inspect_json_valid = ($null -ne $snapshotAnswerInspectJson)
        snapshot_answer_inspect_live_view = if ($null -ne $snapshotAnswerInspectJson -and $null -ne $snapshotAnswerInspectJson.state) { $snapshotAnswerInspectJson.state.live_view } else { $null }
        snapshot_answer_inspect_live_focus = if ($null -ne $snapshotAnswerInspectJson -and $null -ne $snapshotAnswerInspectJson.state) { $snapshotAnswerInspectJson.state.live_focus } else { $null }
        snapshot_resize_bytes = [Text.Encoding]::UTF8.GetByteCount($snapshotResizeRaw)
        snapshot_resize_json_valid = ($null -ne $snapshotResizeJson)
        snapshot_resize_render_us = if ($null -ne $snapshotResizeJson) { $snapshotResizeJson.render_us } else { $null }
        snapshot_resize_rect = if ($null -ne $snapshotResizeJson) { $snapshotResizeJson.rect } else { $null }
        snapshot_mid_busy = if ($null -ne $snapshotMidState) { $snapshotMidState.busy } else { $null }
        snapshot_mid_queued = if ($null -ne $snapshotMidState) { $snapshotMidState.queued } else { $null }
        snapshot_mid_queue = if ($null -ne $snapshotMidState) { @($snapshotMidState.queue) } else { @() }
        snapshot_interrupt_after_ms = $effectiveInterruptAfterMs
        snapshot_busy = if ($null -ne $snapshotState) { $snapshotState.busy } else { $null }
        snapshot_waiting = if ($null -ne $snapshotState) { $snapshotState.waiting } else { $null }
        snapshot_phase = if ($null -ne $snapshotState) { $snapshotState.phase } else { $null }
        snapshot_activity = if ($null -ne $snapshotState) { $snapshotState.activity } else { $null }
        snapshot_activity_kind = if ($null -ne $snapshotState) { $snapshotState.activity_kind } else { $null }
        snapshot_reasoning_expanded = if ($null -ne $snapshotState) { $snapshotState.reasoning_expanded } else { $null }
        snapshot_queued = if ($null -ne $snapshotState) { $snapshotState.queued } else { $null }
        snapshot_live_blocks = if ($null -ne $snapshotState) { $snapshotState.live_blocks } else { $null }
        snapshot_live_focus = if ($null -ne $snapshotState) { $snapshotState.live_focus } else { $null }
        snapshot_panel_kind = if ($null -ne $snapshotJson -and $null -ne $snapshotJson.panel) { $snapshotJson.panel.kind } else { $null }
        snapshot_panel_detail_open = if ($null -ne $snapshotJson -and $null -ne $snapshotJson.panel) { $snapshotJson.panel.detail_open } else { $null }
        snapshot_reasoning_history = if ($null -ne $snapshotState) { $snapshotState.reasoning_history } else { $null }
        snapshot_answer_history = if ($null -ne $snapshotState) { $snapshotState.answer_history } else { $null }
        snapshot_rate = if ($null -ne $snapshotState) { $snapshotState.rate } else { $null }
        snapshot_effort = if ($null -ne $snapshotState) { $snapshotState.effort } else { $null }
        snapshot_has_ridge_marker = ($snapshotRows -match 'RIDGE|RidgeCode|ready')
        snapshot_has_help = ($snapshotRows -match '(?i)help')
        snapshot_has_next_queue = $queueAffordanceObserved
        snapshot_has_reasoning_history = $null -ne $snapshotState -and $snapshotState.reasoning_history -gt 0
        snapshot_has_answer_history = $null -ne $snapshotState -and $snapshotState.answer_history -gt 0
        completion_fixture_requested = $completionMode
        stress_fixture_requested = $stressFixtureRequested
        esc_takeover_requested = [bool]$EscTakeover
        esc_takeover_sent = $sentEsc
        takeover_evidence_satisfied = $takeoverEvidenceSatisfied
        completion_fixture_task_sent = $completionTaskSent
        completion_observed = $completionObserved
        completion_text_observed = $completionTextObserved
        completion_evidence_satisfied = $completionEvidenceSatisfied
        snapshot_path = if ($KeepDiagnostics) { $isolatedSnapshot } else { $null }
        trace = $trace
        trace_path = if ($KeepDiagnostics) { $isolatedTrace } else { $null }
        keylog_event_count = $keylogEvents.Count
        keylog_has_enter = $keylogHasEnter
        keylog_has_ctrl_c = $keylogHasCtrlC
        crossterm_events_observed = $crosstermEventsObserved
        raw_enter_sent = $sentHelp
        busy_fixture_front_sent = $sentFront
        busy_fixture_front_fallback_sent = $frontFallbackSent
        busy_fixture_front_transport = if (-not $BusyFixture) { 'not-applicable' } elseif ($frontFallbackSent) { 'csi-u→legacy-crlf' } else { 'csi-u' }
        busy_fixture_front_observed = $busyFixtureFrontObserved
        live_inspector_requested = [bool]$InspectLive
        reasoning_requested = [bool]$InspectReasoning
        reasoning_sent = $sentReasoning
        reasoning_observed = $reasoningObserved
        answer_inspect_requested = [bool]$InspectAnswer
        answer_inspect_sent = $sentAnswerInspect
        answer_inspect_observed = $answerInspectObserved
        answer_inspect_evidence_satisfied = $answerInspectEvidenceSatisfied
        hold_requested = [bool]$InspectHold
        hold_sent = $sentHold
        hold_observed = $holdObserved
        follow_sent = $sentFollow
        follow_observed = $followObserved
        resize_requested = [bool]$ResizeProbe
        resize_sent = $sentResize
        resize_observed = $resizeObserved
        resize_target_columns = $resizeColumns
        resize_target_rows = $resizeRows
        resize_expected_frame_rows = $resizeFrameRows
        resize_target_count = $resizeTargets.Count
        resize_sent_count = $resizeTargetIndex
        resize_observed_count = $resizeObservedCount
        live_inspector_sent = $sentInspect
        live_inspector_observed = $inspectObserved
        live_inspector_space_sent = $sentInspectSpace
        live_inspector_expanded_observed = $inspectExpandedObserved
        live_inspector_queue_requested = [bool]$InspectQueue
        live_inspector_end_sent = $sentInspectEnd
        live_inspector_delete_sent = $sentInspectDelete
        live_inspector_queue_removed_observed = $inspectQueueRemovedObserved
        attention_queue_switch_sent = $sentQueueSwitch
        attention_queue_observed = $attentionQueueObserved
        attention_live_return_sent = $sentInspectReturn
        attention_live_return_observed = $attentionLiveObserved
        queue_affordance_observed = $queueAffordanceObserved
        alive_after_enter = $aliveAfterEnter
        raw_ctrl_c_twice_sent = $sentInterrupt
    } | ConvertTo-Json -Compress
} finally {
    if ($null -ne $session) { $session.Dispose() }
    if ($null -eq $previousConfig) {
        Remove-Item Env:RIDGE_CONFIG -ErrorAction SilentlyContinue
    } else {
        $env:RIDGE_CONFIG = $previousConfig
    }
    foreach ($name in $isolatedVariables) {
        if ($null -eq $previousVariables[$name]) {
            Remove-Item "Env:$name" -ErrorAction SilentlyContinue
        } else {
            Set-Item "Env:$name" $previousVariables[$name]
        }
    }
    if ($null -eq $previousUserProfile) {
        Remove-Item Env:USERPROFILE -ErrorAction SilentlyContinue
    } else {
        $env:USERPROFILE = $previousUserProfile
    }
    if (-not $KeepDiagnostics) {
        Remove-Item -LiteralPath $isolatedConfig, $isolatedAuth, $isolatedOauth -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $isolatedHome -Recurse -Force -ErrorAction SilentlyContinue
    }
}
