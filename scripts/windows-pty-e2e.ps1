[CmdletBinding()]
param(
    [string]$Binary,
    [int]$Columns = 96,
    [int]$Rows = 24,
    # InspectAnswer waits for a live Answer before the takeover probe. The
    # completion/stress fixtures also exercise full-body rendering, so leave
    # room for the scripted turn, two Ctrl-C bytes, and final frame drain.
    [int]$TimeoutMs = 15000,
    [int]$MaxOutputBytes = 4194304,
    [int]$RenderP95BudgetUs = 16000,
    [int]$RenderMaxBudgetUs = 50000,
    [int]$EnterAfterMs = 500,
    [int]$InterruptAfterMs = 1200,
    [switch]$EscTakeover,
    [switch]$BusyFixture,
    [switch]$StressFixture,
    [switch]$CompletionFixture,
    [switch]$CommandsFixture,
    [switch]$InputFixture,
    [switch]$InspectLive,
    [switch]$InspectQueue,
    [switch]$InspectReasoning,
    [switch]$InspectAnswer,
    [switch]$InspectHold,
    [switch]$ResizeProbe,
    [switch]$KeepDiagnostics,
    [switch]$AllowPartial
)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($Binary)) {
    $Binary = Join-Path $PSScriptRoot '..\target\debug\ridgecode.exe'
}
if ($MaxOutputBytes -lt 1) {
    throw '-MaxOutputBytes must be positive'
}
if ($RenderP95BudgetUs -lt 1 -or $RenderMaxBudgetUs -lt 1) {
    throw '-RenderP95BudgetUs and -RenderMaxBudgetUs must be positive'
}
if ($RenderP95BudgetUs -gt $RenderMaxBudgetUs) {
    throw '-RenderP95BudgetUs cannot exceed -RenderMaxBudgetUs'
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
if ($CommandsFixture -and ($BusyFixture -or $StressFixture -or $CompletionFixture -or $InputFixture)) {
    throw '-CommandsFixture is mutually exclusive with other fixtures'
}
if ($InputFixture -and ($BusyFixture -or $StressFixture -or $CompletionFixture)) {
    throw '-InputFixture is mutually exclusive with other fixtures'
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
$diffMode = [bool]$CompletionFixture
$commandsMode = [bool]$CommandsFixture
$inputMode = [bool]$InputFixture
$answerArchiveMode = [bool]($InspectAnswer -or $CompletionFixture)
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
            public int InheritHandle;
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
            ref SecurityAttributes processAttributes,
            ref SecurityAttributes threadAttributes,
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

        private static void AssertPseudoConsoleOwnsStandardHandles(StartupInfoEx startup) {
            if ((startup.StartupInfo.Flags != 0 &&
                 startup.StartupInfo.Flags != STARTF_USESTDHANDLES) ||
                startup.StartupInfo.StdInput != IntPtr.Zero ||
                startup.StartupInfo.StdOutput != IntPtr.Zero ||
                startup.StartupInfo.StdError != IntPtr.Zero) {
                throw new InvalidOperationException(
                    "ConPTY host pipes must not be attached as child standard handles");
            }
        }

        public static ConPtyNative Start(string applicationName, string currentDirectory, short columns, short rows) {
            var result = new ConPtyNative();
            IntPtr inputRead = IntPtr.Zero;
            IntPtr outputWrite = IntPtr.Zero;
            IntPtr attributeList = IntPtr.Zero;
            SecurityAttributes pipeAttributes = new SecurityAttributes {
                Length = Marshal.SizeOf(typeof(SecurityAttributes)),
                InheritHandle = 1,
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
                // After CreatePseudoConsole, these pipe ends belong exclusively to
                // the HPCON host. Do not also pass them as child std handles: that
                // creates a second consumer and bypasses the pseudo-console path.
                // A redirected host otherwise copies its non-console std
                // handles into the child. Mark the std slots explicitly while
                // keeping all three null: ConPTY supplies the child console
                // handles, and host pipe ends remain single-consumer.
                startup.StartupInfo.Flags = STARTF_USESTDHANDLES;
                startup.StartupInfo.StdInput = IntPtr.Zero;
                startup.StartupInfo.StdOutput = IntPtr.Zero;
                startup.StartupInfo.StdError = IntPtr.Zero;
                startup.AttributeList = attributeList;
                AssertPseudoConsoleOwnsStandardHandles(startup);
                var commandLine = new StringBuilder("\"" + applicationName + "\"");
                var processAttributes = new SecurityAttributes {
                    Length = Marshal.SizeOf(typeof(SecurityAttributes)),
                    InheritHandle = 0,
                };
                var threadAttributes = new SecurityAttributes {
                    Length = Marshal.SizeOf(typeof(SecurityAttributes)),
                    InheritHandle = 0,
                };
                ProcessInformation process;
                Ensure(CreateProcess(
                    null,
                    commandLine,
                    ref processAttributes,
                    ref threadAttributes,
                    false,
                    EXTENDED_STARTUPINFO_PRESENT,
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
$runtimeTimeoutMs = if ($commandsMode) { [Math]::Max($TimeoutMs, 10000) } else { $TimeoutMs }
$startupGraceMs = 4000
$deadline = [DateTime]::UtcNow.AddMilliseconds($runtimeTimeoutMs + $startupGraceMs)
$startupEsc = [char]27
$startupShowMarker = $startupEsc + '[?25h'
$startupReadyAt = $null
$sentHelp = $false
$sentInputProbe = $false
$inputStage = 0
$inputSpaceObserved = $false
$inputBackspaceObserved = $false
$inputDeleteObserved = $false
$inputPasteSent = $false
$inputPasteObserved = $false
$inputPasteKeylogBoundary = 0
$inputPasteTransport = ''
$inputBackend = ''
$inputBackendReason = ''
$inputEnterSent = $false
$inputEnterObserved = $false
$inputEnterObservedAt = $null
$inputFinalLfKeylogBoundary = 0
$inputImmediateSent = $false
$inputImmediateObserved = $false
$inputImmediateObservedAt = $null
$inputImmediateBusyObserved = $false
$inputImmediateKeylogBoundary = 0
$inputProbeObserved = $false
$inputTabObserved = $false
$inputTabKeylogBoundary = 0
$inputShiftTabSent = $false
$inputShiftTabObserved = $false
$inputShiftTabKeylogBoundary = 0
$inputShiftTabTransport = ''
$snapshotInputProbeRaw = ''
$snapshotInputProbeJson = $null
$commandsStage = 0
$commandsLoginObserved = $false
$commandsModelsObserved = $false
$commandsSearchObserved = $false
$commandsEffortObserved = $false
$commandsAnswerObserved = $false
$commandsExitSent = $false
$completionTaskSent = $false
$completionObserved = $false
$completionToolHistorySent = $false
$completionToolDetailSent = $false
$completionToolDetailEndSent = $false
$snapshotCompletionRaw = ''
$snapshotCompletionJson = $null
$sentInterrupt = $false
$sentFront = $false
$sentQueueTail = $false
$sentInspect = $false
$sentInspectSpace = $false
$sentInspectCollapse = $false
$inspectCollapsedObserved = $false
$sentInspectEnd = $false
$sentInspectDelete = $false
$sentQueueSwitch = $false
$sentInspectReturn = $false
$sentReasoning = $false
$reasoningObserved = $false
$sentAnswerInspect = $false
$sentAnswerArchiveInspect = $false
$sentAnswerInspectEnd = $false
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
    # Leave one full redraw window after the CSI-u/legacy fallback front-send;
    # otherwise ConPTY can be interrupted before Queue[2] reaches the pipe.
    [Math]::Max($InterruptAfterMs, $(if ($InspectQueue) { 6000 } elseif ($InspectLive) { 3200 } elseif ($InspectReasoning -or $InspectHold) { 2800 } else { 3200 }))
} elseif ($CompletionFixture) {
    [Math]::Max($InterruptAfterMs, $(if ($InspectAnswer) { 9000 } else { 7000 }))
} elseif ($stressFixtureRequested) {
    [Math]::Max($InterruptAfterMs, $(if ($InspectAnswer) { 9000 } else { 7000 }))
} else {
    $InterruptAfterMs
}
$rawOutput = New-Object 'System.Collections.Generic.List[byte]'
$previousConfig = [Environment]::GetEnvironmentVariable('RIDGECODE_CONFIG', 'Process')
$runId = [Guid]::NewGuid().ToString('N')
$isolatedConfig = Join-Path ([IO.Path]::GetTempPath()) "ridgecode-pty-$runId.json"
$isolatedAuth = Join-Path ([IO.Path]::GetTempPath()) "ridgecode-pty-$runId-auth.json"
$isolatedOauth = Join-Path ([IO.Path]::GetTempPath()) "ridgecode-pty-$runId-oauth.json"
$isolatedHome = Join-Path ([IO.Path]::GetTempPath()) "ridgecode-pty-$runId-home"
$isolatedWorkspace = Join-Path ([IO.Path]::GetTempPath()) "ridgecode-pty-$runId-workspace"
$isolatedSnapshot = Join-Path $isolatedHome '.ridge\frame.json'
$isolatedTrace = Join-Path $isolatedHome '.ridge\tui-trace.log'
$isolatedVariables = @(
    'RIDGECODE_PROVIDER', 'RIDGECODE_MODEL', 'RIDGECODE_BASE_URL', 'RIDGECODE_API_KEY',
    'RIDGECODE_READ_ONLY', 'RIDGECODE_SKIP_PERMISSIONS', 'RIDGECODE_MCP', 'RIDGECODE_AUTH', 'RIDGECODE_OAUTH',
    'RIDGECODE_KEYLOG', 'RIDGECODE_TUI_SNAPSHOT', 'RIDGECODE_TUI_INPUT_DIAGNOSTICS', 'RIDGECODE_FORCE_TUI', 'RIDGECODE_TUI_FIXTURE', 'RIDGECODE_TUI_TRACE', 'RIDGECODE_TUI_KITTY', 'RIDGECODE_TUI_VT_INPUT', 'RIDGECODE_TUI_INSPECT_ANSWER', 'RIDGECODE_TUI_MOUSE_CAPTURE'
)
$previousVariables = @{}
foreach ($name in $isolatedVariables) {
    $previousVariables[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
$previousUserProfile = [Environment]::GetEnvironmentVariable('USERPROFILE', 'Process')

function Remove-ValidatedTempDirectory {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$ExpectedLeaf
    )
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    $fullPath = [IO.Path]::GetFullPath($Path)
    $tempPrefix = $tempRoot.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    $leaf = [IO.Path]::GetFileName($fullPath.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar))
    if ($fullPath.StartsWith($tempPrefix, [StringComparison]::OrdinalIgnoreCase) -and $leaf -eq $ExpectedLeaf) {
        Remove-Item -LiteralPath $fullPath -Recurse -Force -ErrorAction SilentlyContinue
    } else {
        Write-Warning "Refusing to recursively remove non-isolated temp path: $fullPath"
    }
}

function Read-SharedText {
    param([Parameter(Mandatory = $true)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return ''
    }
    $stream = $null
    $reader = $null
    try {
        $share = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
        $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, $share)
        $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8, $true, 4096, $false)
        return $reader.ReadToEnd()
    } finally {
        if ($null -ne $reader) { $reader.Dispose() }
        elseif ($null -ne $stream) { $stream.Dispose() }
    }
}

try {
    New-Item -ItemType Directory -Path $isolatedWorkspace -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $isolatedHome '.ridge') -Force | Out-Null
    $env:USERPROFILE = $isolatedHome
    $env:RIDGECODE_CONFIG = $isolatedConfig
    foreach ($name in $isolatedVariables) { Remove-Item "Env:$name" -ErrorAction SilentlyContinue }
    $env:RIDGECODE_AUTH = $isolatedAuth
    $env:RIDGECODE_OAUTH = $isolatedOauth
    $env:RIDGECODE_PROVIDER = 'openai'
    $env:RIDGECODE_MODEL = 'pty-test-model'
    $env:RIDGECODE_BASE_URL = 'http://127.0.0.1:9/v1'
    $env:RIDGECODE_API_KEY = 'pty-test-key'
    $env:RIDGECODE_KEYLOG = '1'
    $env:RIDGECODE_TUI_SNAPSHOT = $isolatedSnapshot
    # Keep legacy fixtures stable; InputFixture below explicitly proves the
    # raw-VT transport while the production default remains auto.
    $env:RIDGECODE_TUI_VT_INPUT = '0'
    if ($inputMode) {
        $env:RIDGECODE_TUI_INPUT_DIAGNOSTICS = '1'
    }
    $env:RIDGECODE_FORCE_TUI = '1'
    $env:RIDGECODE_TUI_TRACE = $isolatedTrace
    # Native scrollback/selection is the acceptance path.  Isolate the opt-in
    # application mouse-capture switch from the caller's process environment.
    $env:RIDGECODE_TUI_MOUSE_CAPTURE = '0'
    if ($InspectAnswer) {
        $env:RIDGECODE_TUI_INSPECT_ANSWER = '1'
    }
    if ($StressFixture) {
        $env:RIDGECODE_TUI_FIXTURE = 'stress'
    } elseif ($BusyFixture) {
        $env:RIDGECODE_TUI_FIXTURE = 'busy'
        $env:RIDGECODE_TUI_KITTY = '1'
    } elseif ($CompletionFixture) {
        $env:RIDGECODE_TUI_FIXTURE = 'complete'
        # Completion fixture edits only this pre-seeded file in the isolated
        # workspace, keeping PTY evidence hermetic while exercising a real edit.
        $fixtureDirectory = Join-Path $isolatedWorkspace 'src'
        New-Item -ItemType Directory -Path $fixtureDirectory -Force | Out-Null
        $fixtureOldLines = @('fixture old line') +
            @(1..18 | ForEach-Object { 'fixture old detail {0:D2}' -f $_ }) +
            @('fixture old tail marker')
        $fixtureOldString = $fixtureOldLines -join "`n"
        [IO.File]::WriteAllText(
            (Join-Path $fixtureDirectory 'fixture.rs'),
            $fixtureOldString
        )
        $env:RIDGECODE_SKIP_PERMISSIONS = '1'
    } elseif ($CommandsFixture) {
        $env:RIDGECODE_TUI_FIXTURE = 'commands'
        $env:RIDGECODE_TUI_KITTY = '1'
    } elseif ($InputFixture) {
        $env:RIDGECODE_TUI_FIXTURE = 'input'
        # Make the transport assertion deterministic: the fixture proves the
        # raw-VT path and the pure policy matrix covers auto/unset separately.
        $env:RIDGECODE_TUI_VT_INPUT = '1'
    }
    [IO.File]::WriteAllText(
        $isolatedConfig,
        '{"provider":"openai","model":"pty-test-model","base_url":"http://127.0.0.1:9/v1"}'
    )
    $session = [RidgeCode.ConPtyNative]::Start($binaryPath, $isolatedWorkspace, [int16]$Columns, [int16]$Rows)
    while ([DateTime]::UtcNow -lt $deadline -and -not $session.HasExited) {
        $bytes = $session.ReadAvailable()
        if ($bytes.Length -gt 0) {
            if ($rawOutput.Count + $bytes.Length -gt $MaxOutputBytes) {
                throw "ConPTY output exceeded -MaxOutputBytes ($MaxOutputBytes)"
            }
            $rawOutput.AddRange($bytes)
            [void]$text.Append([Text.Encoding]::UTF8.GetString($bytes))
        }
        if ($null -eq $startupReadyAt -and $text.ToString().Contains($startupShowMarker)) {
            $startupReadyAt = [DateTime]::UtcNow
        }
        $elapsed = if ($null -ne $startupReadyAt) {
            ([DateTime]::UtcNow - $startupReadyAt).TotalMilliseconds
        } else {
            -1
        }
        if (-not $sentHelp -and $elapsed -ge $EnterAfterMs) {
            $busyReady = -not $BusyFixture
            if ($BusyFixture -and (Test-Path -LiteralPath $isolatedSnapshot)) {
                try {
                    $busyProbe = [IO.File]::ReadAllText($isolatedSnapshot) | ConvertFrom-Json
                    $busyReady = $null -ne $busyProbe.state -and $busyProbe.state.busy
                } catch {
                    # Retry after the fixture's first busy frame is durable.
                }
            }
            if ($busyReady) {
                if ($inputMode) {
                    # Start a phased probe. Every subsequent physical action
                    # waits for its own snapshot/keylog acknowledgement, so a
                    # final buffer cannot falsely prove an earlier action.
                    $session.Send([Text.Encoding]::UTF8.GetBytes('a b'))
                    $inputStage = 1
                    $sentInputProbe = $true
                    $sentHelp = $true
                } else {
                    $initialProbe = if ($BusyFixture) { 'queued tail message' } else { '/help' }
                    $session.Send([Text.Encoding]::UTF8.GetBytes($initialProbe))
                    $session.Send([byte[]](0x0d))
                    $sentHelp = $true
                    $sentQueueTail = [bool]$BusyFixture
                }
            }
        }
        if ($inputMode -and $sentInputProbe -and -not $inputEnterSent -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                $probe = $parsed.input
                $keylogProbe = Read-SharedText (Join-Path $isolatedHome '.ridge\keylog.txt')
                if ($null -ne $probe) {
                    switch ($inputStage) {
                        1 {
                            if ([string]$probe.buffer -eq 'a b' -and [int]$probe.cursor -eq 3) {
                                $inputSpaceObserved = $true
                                $session.Send([byte[]](0x08))
                                $inputStage = 2
                            }
                        }
                        2 {
                            if ([string]$probe.buffer -eq 'a ' -and [int]$probe.cursor -eq 2) {
                                $inputBackspaceObserved = $true
                                $session.Send([Text.Encoding]::UTF8.GetBytes('b'))
                                $inputStage = 3
                            }
                        }
                        3 {
                            if ([string]$probe.buffer -eq 'a b' -and [int]$probe.cursor -eq 3) {
                                # Raw DEL normalizes to Backspace on legacy
                                # ConPTY. Reinsert `b` first so deletion has a
                                # distinct, observable effect.
                                $session.Send([byte[]](0x7f))
                                $inputStage = 4
                            }
                        }
                        4 {
                            if ([string]$probe.buffer -eq 'a ' -and [int]$probe.cursor -eq 2) {
                                $inputDeleteObserved = $true
                                $session.Send([byte[]](0x08))
                                $inputStage = 5
                            }
                        }
                        5 {
                            if ([string]$probe.buffer -eq 'a' -and [int]$probe.cursor -eq 1) {
                                $inputTabKeylogBoundary = $keylogProbe.Length
                                $session.Send([byte[]](0x09))
                                $inputStage = 6
                            }
                        }
                        6 {
                            $tabTail = if ($keylogProbe.Length -gt $inputTabKeylogBoundary) {
                                $keylogProbe.Substring($inputTabKeylogBoundary)
                            } else { '' }
                            if ([string]$probe.buffer -eq 'a' -and [int]$probe.cursor -eq 1 -and
                                $tabTail -match "(?m)code:\s*(?:Tab|Char\('\\t'\))") {
                                $inputTabObserved = $true
                                $inputShiftTabKeylogBoundary = $keylogProbe.Length
                                # Standard VT/xterm Shift-Tab is ESC [ Z.
                                $session.Send([byte[]](0x1b, 0x5b, 0x5a))
                                $inputShiftTabSent = $true
                                $inputStage = 7
                            }
                        }
                        7 {
                            $shiftTabTail = if ($keylogProbe.Length -gt $inputShiftTabKeylogBoundary) {
                                $keylogProbe.Substring($inputShiftTabKeylogBoundary)
                            } else { '' }
                            $shiftTabEvent = $shiftTabTail -match '(?m)code:\s*BackTab'
                            $shiftTabRawFallback = $shiftTabTail -match "(?s)code:\s*Char\('\['\).*?code:\s*Char\('Z'\)"
                            if ([string]$probe.buffer -eq 'a' -and [int]$probe.cursor -eq 1 -and
                                ($shiftTabEvent -or $shiftTabRawFallback)) {
                                $inputShiftTabObserved = $true
                                $inputShiftTabTransport = if ($shiftTabEvent) { 'backtab-event' } else { 'raw-vt-fallback' }
                                $inputPasteKeylogBoundary = $keylogProbe.Length
                                # One write preserves the CSI/OSC paste
                                # envelope across the ConPTY byte boundary.
                                $inputProbeBytes = New-Object 'System.Collections.Generic.List[byte]'
                                [void]$inputProbeBytes.AddRange([byte[]](0x1b, 0x5b, 0x32, 0x30, 0x30, 0x7e))
                                [void]$inputProbeBytes.AddRange([Text.Encoding]::UTF8.GetBytes('x'))
                                [void]$inputProbeBytes.AddRange([byte[]](0x1b, 0x5b, 0x33, 0x31, 0x6d))
                                [void]$inputProbeBytes.AddRange([Text.Encoding]::UTF8.GetBytes('y'))
                                [void]$inputProbeBytes.AddRange([byte[]](0x1b, 0x5d, 0x30, 0x3b, 0x74, 0x69, 0x74, 0x6c, 0x65, 0x07))
                                [void]$inputProbeBytes.AddRange([Text.Encoding]::UTF8.GetBytes('z'))
                                [void]$inputProbeBytes.AddRange([byte[]](0x1b, 0x5b, 0x32, 0x30, 0x31, 0x7e))
                                $session.Send($inputProbeBytes.ToArray())
                                $inputPasteSent = $true
                                $inputStage = 8
                            }
                        }
                        8 {
                            $pasteTail = if ($keylogProbe.Length -gt $inputPasteKeylogBoundary) {
                                $keylogProbe.Substring($inputPasteKeylogBoundary)
                            } else { '' }
                            $pasteEvent = $pasteTail -match '(?m)^Paste\('
                            $pasteRawStart = $pasteTail -match "(?s)code:\s*Char\('\['\).*?code:\s*Char\('2'\).*?code:\s*Char\('0'\).*?code:\s*Char\('0'\).*?code:\s*Char\('~'\)"
                            $pasteRawEnd = $pasteTail -match "(?s)code:\s*Char\('\['\).*?code:\s*Char\('2'\).*?code:\s*Char\('0'\).*?code:\s*Char\('1'\).*?code:\s*Char\('~'\)"
                            if ([string]$probe.buffer -eq 'axyz' -and [int]$probe.cursor -eq 4 -and
                                ($pasteEvent -or ($pasteRawStart -and $pasteRawEnd))) {
                                $snapshotInputProbeRaw = $candidate
                                $snapshotInputProbeJson = $parsed
                                $inputProbeObserved = $true
                                $inputPasteObserved = $true
                                $inputPasteTransport = if ($pasteEvent) { 'paste-event' } else { 'raw-bracketed-fallback' }
                                $inputFinalLfKeylogBoundary = $keylogProbe.Length
                                $session.Send([byte[]](0x0a))
                                $inputEnterSent = $true
                                $inputStage = 9
                            }
                        }
                    }
                }
            } catch {
                # Retry while the frame writer is replacing the snapshot.
            }
        }
        if ($inputMode -and $inputEnterSent -and -not $inputEnterObserved) {
            $keylogPath = Join-Path $isolatedHome '.ridge\keylog.txt'
            $keylogAfterFinalLf = Read-SharedText $keylogPath
            if ($keylogAfterFinalLf.Length -gt $inputFinalLfKeylogBoundary) {
                $inputFinalLfKeylogTail = $keylogAfterFinalLf.Substring($inputFinalLfKeylogBoundary)
                # Only an input event appended after the final LF proves that
                # the submit key reached the application. Earlier Enter/CR/LF
                # events from before this boundary must not satisfy it.
                if ($inputFinalLfKeylogTail -match "(?m)code:\s*(?:Enter|Char\('\s*\\[rn]\s*'\))") {
                    $inputEnterObserved = $true
                    $inputEnterObservedAt = [DateTime]::UtcNow
                }
            }
        }
        if ($inputMode -and $inputEnterObserved -and -not $inputImmediateSent -and
            (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $immediateReady = [IO.File]::ReadAllText($isolatedSnapshot) | ConvertFrom-Json
                if ($null -ne $immediateReady.state -and -not $immediateReady.state.busy -and
                    [string]$immediateReady.state.phase -eq 'completed' -and
                    [string]$immediateReady.input.buffer -eq '') {
                    $immediateKeylog = Read-SharedText (Join-Path $isolatedHome '.ridge\keylog.txt')
                    $inputImmediateKeylogBoundary = $immediateKeylog.Length
                    $immediateBytes = New-Object 'System.Collections.Generic.List[byte]'
                    [void]$immediateBytes.AddRange([byte[]](0x1b, 0x5b, 0x32, 0x30, 0x30, 0x7e))
                    [void]$immediateBytes.AddRange([Text.Encoding]::UTF8.GetBytes('instant paste'))
                    [void]$immediateBytes.AddRange([byte[]](0x1b, 0x5b, 0x32, 0x30, 0x31, 0x7e, 0x0d, 0x0a))
                    # One physical ConPTY write is the regression boundary:
                    # Paste must precede exactly one semantic Enter.
                    $session.Send($immediateBytes.ToArray())
                    $inputImmediateSent = $true
                    $inputStage = 10
                }
            } catch {
                # Retry while the frame writer is replacing the snapshot.
            }
        }
        if ($inputMode -and $inputImmediateSent -and -not $inputImmediateObserved) {
            $immediateLog = Read-SharedText (Join-Path $isolatedHome '.ridge\keylog.txt')
            $immediateTail = if ($immediateLog.Length -gt $inputImmediateKeylogBoundary) {
                $immediateLog.Substring($inputImmediateKeylogBoundary)
            } else { '' }
            $immediatePasteCount = @([regex]::Matches($immediateTail, '(?m)^Paste\("instant paste"\)')).Count
            $immediateEnterCount = @([regex]::Matches($immediateTail, "(?m)^Key\(KeyEvent \{ code: (?:Enter|Char\('\\[rn]'\))")).Count
            if (Test-Path -LiteralPath $isolatedSnapshot) {
                try {
                    $immediateFrame = [IO.File]::ReadAllText($isolatedSnapshot) | ConvertFrom-Json
                    if ($null -ne $immediateFrame.state -and $immediateFrame.state.busy -and
                        [string]$immediateFrame.input.buffer -eq '') {
                        $inputImmediateBusyObserved = $true
                    }
                } catch {
                    # Retry while the frame writer is replacing the snapshot.
                }
            }
            if ($inputImmediateBusyObserved -and $immediatePasteCount -eq 1 -and $immediateEnterCount -eq 1) {
                $inputImmediateObserved = $true
                $inputImmediateObservedAt = [DateTime]::UtcNow
                $inputStage = 11
            }
        }
        if ($commandsMode) {
            $commandsSnapshot = $null
            if (Test-Path -LiteralPath $isolatedSnapshot) {
                try {
                    $commandsSnapshot = [IO.File]::ReadAllText($isolatedSnapshot) | ConvertFrom-Json
                } catch {
                    # Retry while the frame writer is replacing the snapshot.
                }
            }
            $commandsPanelKind = if ($null -ne $commandsSnapshot -and $null -ne $commandsSnapshot.panel) {
                [string]$commandsSnapshot.panel.kind
            } else {
                ''
            }
            $commandsRows = if ($null -ne $commandsSnapshot) { @($commandsSnapshot.rows) -join ' ' } else { '' }
            switch ($commandsStage) {
                0 {
                    if ($sentHelp -and $elapsed -ge ($EnterAfterMs + 250)) {
                        $session.Send([Text.Encoding]::UTF8.GetBytes('/login'))
                        $session.Send([byte[]](0x0d))
                        $commandsStage = 1
                    }
                }
                1 {
                    if ($commandsPanelKind -eq 'Login') {
                        $commandsLoginObserved = $true
                        $session.Send([byte[]](0x1b))
                        $commandsStage = 2
                    }
                }
                2 {
                    if ($commandsLoginObserved -and $elapsed -ge ($EnterAfterMs + 650)) {
                        $session.Send([Text.Encoding]::UTF8.GetBytes('/provider'))
                        $session.Send([byte[]](0x0d))
                        $commandsStage = 3
                    }
                }
                3 {
                    if ($commandsPanelKind -eq 'Models') {
                        $commandsModelsObserved = $true
                        $session.Send([Text.Encoding]::UTF8.GetBytes('glm'))
                        $commandsStage = 4
                    }
                }
                4 {
                    if ($commandsPanelKind -eq 'Models' -and $null -ne $commandsSnapshot.panel -and
                        [string]$commandsSnapshot.panel.query -match '(?i)glm') {
                        $commandsSearchObserved = $true
                        $session.Send([byte[]](0x0d))
                        $commandsStage = 5
                    }
                }
                5 {
                    if ($commandsPanelKind -eq 'Effort') {
                        $commandsEffortObserved = $true
                        $session.Send([byte[]](0x1b))
                        $commandsStage = 6
                    }
                }
                6 {
                    if ($commandsEffortObserved -and $elapsed -ge ($EnterAfterMs + 1100)) {
                        $session.Send([Text.Encoding]::UTF8.GetBytes('/answer'))
                        $session.Send([byte[]](0x0d))
                        $commandsStage = 7
                    }
                }
                7 {
                    if ($commandsPanelKind -eq 'Answers' -and $null -ne $commandsSnapshot.panel -and
                        $commandsSnapshot.panel.detail_open -and $commandsRows -match 'COMPLETE BODY TAIL') {
                        $commandsAnswerObserved = $true
                        $session.Send([byte[]](0x03))
                        Start-Sleep -Milliseconds 80
                        $session.Send([byte[]](0x03))
                        $commandsExitSent = $true
                        $sentInterrupt = $true
                        $aliveAfterEnter = $true
                        $commandsStage = 8
                    }
                }
            }
        }
        if ($completionMode -and $sentHelp -and -not $completionTaskSent -and $elapsed -ge 850) {
            $session.Send([Text.Encoding]::UTF8.GetBytes('completion fixture task'))
            $session.Send([byte[]](0x0d))
            $completionTaskSent = $true
        }
        if ($answerArchiveMode -and $completionTaskSent -and -not $sentAnswerInspect -and (Test-Path -LiteralPath $isolatedSnapshot)) {
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
        if ($answerArchiveMode -and ($sentAnswerInspect -or $sentAnswerArchiveInspect) -and
            -not $answerInspectObserved -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                $liveAnswerFocused = $null -ne $parsed.state -and $parsed.state.busy -and
                    $parsed.state.live_view -eq 'hold' -and $parsed.state.live_focus -match '^answer:'
                $archivedAnswerExpanded = $null -ne $parsed.state -and
                    $parsed.state.answer_history -ge 1 -and $null -ne $parsed.panel -and
                    $parsed.panel.kind -eq 'Answers' -and $parsed.panel.detail_open
                if ($liveAnswerFocused -or $archivedAnswerExpanded) {
                    $snapshotAnswerInspectRaw = $candidate
                    $snapshotAnswerInspectJson = $parsed
                    $answerInspectObserved = $true
                }
            } catch {
                # Retry after the frame writer leaves its transient state.
            }
        }
        if ($answerArchiveMode -and $completionObserved -and -not $answerInspectObserved -and
            -not $sentAnswerArchiveInspect) {
            # If the live Answer settled before the first Ctrl+A snapshot, use
            # the same shortcut after completion to open the retained full
            # Answer archive and verify its expandable detail surface.
            $session.Send([byte[]](0x01))
            $sentAnswerArchiveInspect = $true
        }
        if ($answerArchiveMode -and $completionObserved -and -not $sentAnswerInspectEnd -and
            (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.panel -and $parsed.panel.kind -eq 'Answers' -and
                    $parsed.panel.detail_open) {
                    # End scrolls an expanded answer detail to its retained tail;
                    # the raw ConPTY stream must therefore expose middle/tail
                    # table and fenced-code content, not only archive metadata.
                    $session.Send([byte[]](0x1b, 0x5b, 0x46))
                    $sentAnswerInspectEnd = $true
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
        if ($CompletionFixture -and $completionObserved -and $answerInspectObserved -and
            -not $completionToolHistorySent) {
            # The completion fixture's edit is initially folded in the live
            # rail. Open Tool history so the PTY captures its real diff detail.
            $session.Send([byte[]](0x0f))
            $completionToolHistorySent = $true
        }
        if ($CompletionFixture -and $completionToolHistorySent -and
            -not $completionToolDetailSent -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.panel -and $parsed.panel.kind -eq 'ToolHistory') {
                    # Enter expands the selected edit row; End then exposes the
                    # retained tail while the earlier frame carries old lines.
                    $session.Send([byte[]](0x0d))
                    $completionToolDetailSent = $true
                }
            } catch {
                # Retry after the history panel frame is durable.
            }
        }
        if ($CompletionFixture -and $completionToolDetailSent -and
            -not $completionToolDetailEndSent -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.panel -and $parsed.panel.kind -eq 'ToolHistory' -and
                    $parsed.panel.detail_open) {
                    $session.Send([byte[]](0x1b, 0x5b, 0x46))
                    $completionToolDetailEndSent = $true
                }
            } catch {
                # Retry after the detail frame is durable.
            }
        }
        if ($BusyFixture -and $sentHelp -and -not $sentFront -and $elapsed -ge 850) {
            $session.Send([Text.Encoding]::UTF8.GetBytes('/front'))
            # BusyFixture opts into Kitty disambiguation.  Exercise the real
            # physical Ctrl+Enter spelling instead of relying on a platform's
            # CR/LF-to-KeyCode fallback; input.rs still normalizes that fallback
            # for legacy Windows terminals.
            # Feed the complete Kitty CSI-u spelling. The application only
            # decodes CSI after an explicit ESC, so ordinary text such as
            # "[A" remains literal instead of becoming navigation.
            $session.Send([byte[]](0x1b, 0x5b, 0x31, 0x33, 0x3b, 0x35, 0x75))
            $sentFront = $true
        }
        if ($BusyFixture -and $sentFront -and -not $sentQueueTail -and $elapsed -ge 1000) {
            $session.Send([Text.Encoding]::UTF8.GetBytes('queued tail message'))
            $session.Send([byte[]](0x0d))
            $sentQueueTail = $true
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
        if ($ResizeProbe -and $resizeTargetIndex -lt $resizeTargets.Count -and
            ($resizeTargetIndex -eq 0 -or $resizeObservedCount -ge $resizeTargetIndex) -and
            (-not $CompletionFixture -or $answerInspectObserved) -and
            $elapsed -ge $resizeTargets[$resizeTargetIndex].at) {
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
        if ($BusyFixture -and $InspectLive -and $sentFront -and -not $sentInspect -and
            $elapsed -ge 1700 -and (-not $InspectQueue -or $null -ne $snapshotMidJson -or $elapsed -ge 3600)) {
            # Exercise the complete Kitty spelling because raw Alt+I may lose
            # its modifier when Windows emits separate INPUT_RECORD entries.
            $session.Send([Text.Encoding]::ASCII.GetBytes(([char]27 + '[105;3u')))
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
        if ($InspectQueue -and $inspectExpandedObserved -and -not $sentInspectCollapse -and $elapsed -ge 2150) {
            # Collapse details before End; while details are open End scrolls
            # the detail body instead of selecting the last actionable row.
            $session.Send([byte[]](0x20))
            $sentInspectCollapse = $true
        }
        if ($sentInspectCollapse -and -not $inspectCollapsedObserved -and (Test-Path -LiteralPath $isolatedSnapshot)) {
            try {
                $candidate = [IO.File]::ReadAllText($isolatedSnapshot)
                $parsed = $candidate | ConvertFrom-Json
                if ($null -ne $parsed.panel -and $parsed.panel.kind -match '^(Live|Audit)$' -and
                    -not $parsed.panel.detail_open) {
                    $inspectCollapsedObserved = $true
                }
            } catch {
                # Retry after the frame write completes.
            }
        }
        if ($InspectQueue -and $inspectCollapsedObserved -and -not $sentInspectEnd -and $elapsed -ge 2250) {
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
                $expectedQueued = if ($null -ne $snapshotMidJson -and $null -ne $snapshotMidJson.state) {
                    [Math]::Max(0, [int]$snapshotMidJson.state.queued - 1)
                } elseif ($null -ne $snapshotInspectJson -and $null -ne $snapshotInspectJson.state) {
                    [Math]::Max(0, [int]$snapshotInspectJson.state.queued - 1)
                } else {
                    -1
                }
                if ($null -ne $parsed.state -and $parsed.state.queued -eq $expectedQueued -and
                    $null -ne $parsed.panel -and $parsed.panel.kind -match '^(Live|Audit)$') {
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
            # Reuse the complete Kitty spelling so Queue -> Inspector retains
            # Alt and never inserts a literal `i` into panel search.
            $session.Send([Text.Encoding]::ASCII.GetBytes(([char]27 + '[105;3u')))
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
                    $history = if ($null -ne $parsed.state) { @($parsed.state.activity_history) } else { @() }
                    $frontAction = $history | Where-Object { $_.text -like 'front-queued*' }
                    $queue = if ($null -ne $parsed.state) { @($parsed.state.queue) } else { @() }
                    if ($null -ne $parsed.state -and $parsed.state.busy -and
                        $parsed.state.queued -ge 2 -and $queue -contains '/front' -and
                        $queue -contains 'queued tail message' -and $null -ne $frontAction) {
                        $snapshotMidRaw = $candidate
                        $snapshotMidJson = $parsed
                    }
                } catch {
                    # The writer may be between truncate/write; retry next loop.
                }
            }
        }
        if ($BusyFixture -and $sentFront -and -not $frontFallbackSent -and $elapsed -ge 2200) {
            # Some Windows ConPTY/INPUT_RECORD hosts consume CSI-u without
            # surfacing a crossterm KeyEvent.  Keep the attempt observable, then
            # fall back to the physical CR/LF spelling that those hosts expose
            # as Ctrl+Enter.  Do not send both when the queue frame proves CSI-u.
            $frontSeen = $false
            if (Test-Path -LiteralPath $isolatedSnapshot) {
                try {
                    $probe = [IO.File]::ReadAllText($isolatedSnapshot) | ConvertFrom-Json
                    $probeHistory = if ($null -ne $probe.state) { @($probe.state.activity_history) } else { @() }
                    $frontAction = $probeHistory | Where-Object { $_.text -like 'front-queued*' }
                    $frontSeen = $null -ne $probe.state -and $null -ne $frontAction
                } catch {
                    # Retry once more after the writer leaves its transient state.
                }
            }
            if (-not $frontSeen) {
                $session.Send([byte[]](0x0a))
                $frontFallbackSent = $true
            }
        }
        $inputTakeoverReady = -not $inputMode
        if ($inputMode -and $inputImmediateObserved -and $null -ne $inputImmediateObservedAt) {
            $inputTakeoverReady = ([DateTime]::UtcNow - $inputImmediateObservedAt).TotalMilliseconds -ge 120
        }
        if (-not $commandsMode -and $sentHelp -and -not $sentInterrupt -and
            $elapsed -ge $effectiveInterruptAfterMs -and $inputTakeoverReady) {
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
        if ($rawOutput.Count + $bytes.Length -gt $MaxOutputBytes) {
            throw "ConPTY output exceeded -MaxOutputBytes ($MaxOutputBytes)"
        }
        $rawOutput.AddRange($bytes)
        [void]$text.Append([Text.Encoding]::UTF8.GetString($bytes))
    }
    $keylogPath = Join-Path $isolatedHome '.ridge\keylog.txt'
    $keylog = Read-SharedText $keylogPath
    $keylogEvents = @($keylog -split "`r?`n" | Where-Object { $_.Trim().Length -gt 0 })
    $backendLine = $keylogEvents | Where-Object { $_ -match '^backend:\s*(\S+)\s+reason=(\S+)' } | Select-Object -Last 1
    if ($null -ne $backendLine -and $backendLine -match '^backend:\s*(\S+)\s+reason=(\S+)') {
        $inputBackend = $Matches[1]
        $inputBackendReason = $Matches[2]
    }
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
    if ($InspectAnswer -and -not $answerInspectObserved -and $null -ne $snapshotJson -and
        $null -ne $snapshotJson.state -and $snapshotJson.state.answer_history -ge 1 -and
        $null -ne $snapshotJson.panel -and $snapshotJson.panel.kind -eq 'Answers' -and
        $snapshotJson.panel.detail_open) {
        # The final drained frame is authoritative when a completed answer
        # settled between the live poll and the next Ctrl+A inspection poll.
        $snapshotAnswerInspectRaw = $snapshotRaw
        $snapshotAnswerInspectJson = $snapshotJson
        $answerInspectObserved = $true
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
    $snapshotTelemetry = if ($null -ne $snapshotJson) {
        $snapshotJson.telemetry
    } else {
        $null
    }
    $renderFrameSequence = if ($null -ne $snapshotTelemetry) {
        [long]$snapshotTelemetry.frame_sequence
    } else {
        0
    }
    $renderSampleCount = if ($null -ne $snapshotTelemetry) {
        [long]$snapshotTelemetry.render_sample_count
    } else {
        0
    }
    $renderP95Us = if ($null -ne $snapshotTelemetry) {
        [long]$snapshotTelemetry.render_p95_us
    } else {
        0
    }
    $renderMaxUs = if ($null -ne $snapshotTelemetry) {
        [long]$snapshotTelemetry.render_max_us
    } else {
        0
    }
    $renderSamplesTruncated = if ($null -ne $snapshotTelemetry) {
        [bool]$snapshotTelemetry.render_samples_truncated
    } else {
        $true
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
        @($snapshotMidState.activity_history | Where-Object { $_.text -like 'front-queued*' }).Count -gt 0
    } else {
        $false
    }
    $snapshotMidRows = if ($null -ne $snapshotMidJson -and $null -ne $snapshotMidJson.rows) {
        @($snapshotMidJson.rows) -join "`n"
    } else {
        ''
    }
    $visibleQueued = if ($null -ne $snapshotMidState) {
        @($snapshotMidState.queue | Where-Object {
                $snapshotMidRows -match [regex]::Escape([string]$_)
            }).Count
    } else {
        0
    }
    $queueAffordanceObserved = $visibleQueued -gt 0 -or ($snapshotRows -match '(?i)next')
    $queueEvidenceSatisfied = -not $BusyFixture -or $queueAffordanceObserved
    $inspectEvidenceSatisfied = -not $InspectLive -or ($inspectObserved -and $inspectExpandedObserved)
    $inspectQueueEvidenceSatisfied = -not $InspectQueue -or ($inspectQueueRemovedObserved -and $attentionQueueObserved -and $attentionLiveObserved)
    $reasoningEvidenceSatisfied = -not $InspectReasoning -or $reasoningObserved
    $answerInspectEvidenceSatisfied = -not $InspectAnswer -or $answerInspectObserved
    $resizeEvidenceSatisfied = -not $ResizeProbe -or $resizeObserved
    $trace = if (Test-Path -LiteralPath $isolatedTrace) {
        [IO.File]::ReadAllText($isolatedTrace)
    } else {
        ''
    }
    # ConPTY captures cursor-addressed cells, so terminal updates may split a
    # word with CSI moves.  Strip ANSI first, then use whitespace-tolerant
    # probes below; decode the complete byte stream once so a UTF-8 glyph split
    # across ReadAvailable chunks cannot become mojibake in the final probes.
    $decodedOutput = [Text.Encoding]::UTF8.GetString($rawOutput.ToArray())
    $ansiPattern = '\x1B(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1B\\))'
    $plain = [regex]::Replace($decodedOutput, $ansiPattern, '')
    if ($BusyFixture -and -not $busyFixtureFrontObserved -and
        $plain -match 'front-queued' -and $plain -match '/front') {
        # Cursor-addressed output is authoritative when the replace-on-write
        # frame snapshot misses the short Queue[2] transition.
        $busyFixtureFrontObserved = $true
    }
    if ($BusyFixture -and -not $queueAffordanceObserved -and
        $plain -match 'Queue\s*\[2\]' -and $plain -match 'queued tail message' -and
        $plain -match '/front') {
        $queueAffordanceObserved = $true
        $queueEvidenceSatisfied = $true
    }
    # Startup contract: the standalone reference canvas must be the first
    # application surface.  Host-generated ConPTY setup bytes may precede it,
    # but config/provider logs must follow the animation hand-off marker.
    $startupText = $decodedOutput
    $startupHideIndex = $startupText.IndexOf($startupEsc + '[?25l')
    $startupClearIndex = $startupText.IndexOf($startupEsc + '[2J', [Math]::Max(0, $startupHideIndex))
    $startupHomeIndex = $startupText.IndexOf($startupEsc + '[H', [Math]::Max(0, $startupClearIndex))
    $startupRgbIndex = $startupText.IndexOf($startupEsc + '[38;2;', [Math]::Max(0, $startupHomeIndex))
    # ConPTY may emit a title/blank initialization frame after the first home
    # cursor move. Anchor line counting to the home immediately before the
    # first RGB cell so the check measures the actual splash canvas.
    $startupFrameHomeIndex = if ($startupRgbIndex -ge 0) {
        $startupText.LastIndexOf($startupEsc + '[H', $startupRgbIndex)
    } else {
        -1
    }
    $startupFrameNextHomeIndex = if ($startupFrameHomeIndex -ge 0) {
        $startupText.IndexOf($startupEsc + '[H', $startupFrameHomeIndex + 3)
    } else {
        -1
    }
    $startupShowIndex = $startupText.IndexOf($startupShowMarker, [Math]::Max(0, $startupRgbIndex))
    $startupLogIndex = $startupText.IndexOf('[ridgecode]', [Math]::Max(0, $startupShowIndex))
    $startupFrameLineBreaks = if ($startupFrameHomeIndex -ge 0 -and $startupFrameNextHomeIndex -gt $startupFrameHomeIndex) {
        ([regex]::Matches(
                $startupText.Substring($startupFrameHomeIndex, $startupFrameNextHomeIndex - $startupFrameHomeIndex),
                '\r?\n'
            )).Count
    } else {
        0
    }
    $startupExpectedFrameLineBreaks = [Math]::Max(8, $Rows - 1) - 1
    $startupAnimationEvidenceSatisfied =
        $startupHideIndex -ge 0 -and
        $startupClearIndex -gt $startupHideIndex -and
        $startupHomeIndex -gt $startupClearIndex -and
        $startupRgbIndex -gt $startupHomeIndex -and
        $startupShowIndex -gt $startupRgbIndex -and
        $startupLogIndex -gt $startupShowIndex -and
        $startupFrameLineBreaks -eq $startupExpectedFrameLineBreaks
    # ConPTY cursor-addressed redraws can insert cell separators or split a
    # word across writes.  Keep a compact ASCII probe for fixture markers;
    # snapshot rows below remain the authoritative visible-frame evidence.
    $probePlain = [regex]::Replace($plain, '[^A-Za-z0-9]+', '').ToLowerInvariant()
    # Main TUI must not enable mouse reporting: native terminal wheel/drag
    # selection belongs to the host scrollback.  The fullscreen editor is an
    # explicit opt-in path and is not exercised by this native-scroll probe.
    $mouseCaptureEnableObserved = $decodedOutput -match '\x1B\[\?(?:1000|1002|1003|1006)h'
    $mouseCaptureDisableObserved = $decodedOutput -match '\x1B\[\?1006l\x1B\[\?1015l\x1B\[\?1003l\x1B\[\?1002l\x1B\[\?1000l'
    # Raw-VT owns a VT byte stream, so prove its explicit mouse-reporting
    # teardown.  Legacy Crossterm owns Win32 INPUT_RECORD mode instead; its
    # ConPTY console handle may not advertise VTP and therefore must not emit
    # ANSI disable bytes.  The native_input host-mode unit contract proves the
    # mouse bit is cleared for that backend, while this stream proves no VT
    # mouse enable leaked into the host.
    $nativeMousePathSatisfied = if ($inputBackend -eq 'raw-vt') {
        -not $mouseCaptureEnableObserved -and $mouseCaptureDisableObserved
    } elseif ($inputBackend -eq 'crossterm') {
        -not $mouseCaptureEnableObserved
    } else {
        -not $mouseCaptureEnableObserved -and $mouseCaptureDisableObserved
    }
    # A Windows raw Ctrl+Space byte commonly arrives as a press/release pair
    # in one ConPTY read.  The application correctly renders the momentary
    # HOLD frame, then returns to FOLLOW on release before the polling loop can
    # capture a JSON frame.  Accept that visible HOLD marker plus final FOLLOW
    # as the same contract, while still preferring the direct snapshot path.
    $holdFrameObserved = ($plain -match '(?i)HOLD\s*·') -or
        ($probePlain -match 'hold(?:model|thinking|answer|tool|waiting|reasoning)')
    if ($InspectHold -and -not $holdObserved -and $holdFrameObserved) {
        $holdObserved = $true
    }
    if ($InspectHold -and $holdObserved -and -not $followObserved -and
        $null -ne $snapshotState -and $snapshotState.live_view -eq 'follow') {
        $followObserved = $true
    }
    $holdEvidenceSatisfied = -not $InspectHold -or ($holdObserved -and $followObserved)
    $inputEvidenceSatisfied = -not $inputMode -or (
        $inputSpaceObserved -and $inputBackspaceObserved -and $inputDeleteObserved -and
        $inputProbeObserved -and $inputPasteObserved -and $inputEnterSent -and
        $inputEnterObserved -and $inputTabObserved -and $inputShiftTabObserved -and
        $inputImmediateSent -and $inputImmediateObserved -and $inputImmediateBusyObserved -and
        $inputBackend -eq 'raw-vt'
    )
    $outputBudgetSatisfied = $session.BytesRead -le $MaxOutputBytes
    $renderBudgetSatisfied = $renderFrameSequence -ge 1 -and
        $renderSampleCount -ge 1 -and
        -not $renderSamplesTruncated -and
        $renderP95Us -le $RenderP95BudgetUs -and
        $renderMaxUs -le $RenderMaxBudgetUs
    $rawOutputPath = Join-Path $isolatedHome '.ridge\pty-output.bin'
    if ($KeepDiagnostics) {
        [IO.File]::WriteAllBytes($rawOutputPath, $rawOutput.ToArray())
    }
    $completionReasoningObserved = if ($stressFixtureRequested) {
        ($plain -match 'STRESS_REASONING_END') -or ($probePlain -match 'stressreasoningend')
    } else {
        ($plain -match 'fixture\s*reasoning\s*:\s*completed\s*path\s*remains\s*inspectable') -or
            ($snapshotRows -match 'fixture\s*reasoning\s*:\s*completed\s*path') -or
            ($probePlain -match 'fixturereasoningcompletedpathremainsinspectable')
    }
    $completionReasoningTailObserved = if ($stressFixtureRequested) {
        ($plain -match 'STRESS_REASONING_END') -or ($probePlain -match 'stressreasoningend')
    } else {
        ($plain -match 'fixture\s+reasoning\s+tail\s+marker\s+24') -or
            ($snapshotRows -match 'fixture\s+reasoning\s+tail\s+marker') -or
            ($probePlain -match 'fixturereasoningtailmarker24')
    }
    $completionAnswerObserved = if ($stressFixtureRequested) {
        ($plain -match 'STRESS_ANSWER_BEGIN') -or ($probePlain -match 'stressanswerbegin')
    } else {
        ($plain -match 'fixture\s*answer\s*:\s*final\s*response\s*reached\s*scrollback') -or
            ($snapshotRows -match 'fixture\s*answer\s*:\s*final\s*response') -or
            ($probePlain -match 'fixtureanswerfinalresponsereachedscrollback')
    }
    $completionTextObserved = -not $completionMode -or ($completionReasoningObserved -and $completionAnswerObserved)
    $diffPathObserved = ($plain -match 'src[\\/]fixture\.rs') -or ($probePlain -match 'srcfixture\.rs')
    $diffRemovedObserved = ($plain -match 'fixture\s+old\s+line') -or ($probePlain -match 'fixtureoldline')
    $diffAddedObserved = ($plain -match 'fixture\s+new\s+line') -or ($probePlain -match 'fixturenewline')
    $diffRemovedTailObserved = ($plain -match 'fixture\s+old\s+tail\s+marker') -or ($probePlain -match 'fixtureoldtailmarker')
    $diffAddedTailObserved = ($plain -match 'fixture\s+new\s+tail\s+marker') -or ($probePlain -match 'fixturenewtailmarker')
    $toolFoldObserved = ($plain -match '\+\s*\d+\s+lines\s*\(Ctrl\+T\s+to\s+view(?:\s+[┃▌┆┊│╰])?\s+transcript\)') -or
        ($probePlain -match '\d+linesctrlttoviewtranscript')
    # Ratatui/ConPTY may serialize the continuation cell of each wide glyph as
    # a blank; accept those display-cell blanks without weakening text order.
    $answerTableObserved = (($plain -match '\u9879\s*\u76EE') -and ($plain -match '\u4E2D\s*\u6587\s*\u81EA\s*\u9002\s*\u5E94')) -or
        (($probePlain -match '\u9879\s*\u76EE') -and ($probePlain -match '\u4E2D\s*\u6587\s*\u81EA\s*\u9002\s*\u5E94'))
    $answerHighlightObserved = (($plain -match 'ridgecode') -and ($plain -match 'rendered\s*=\s*true')) -or
        (($probePlain -match 'ridgecode') -and ($probePlain -match 'rendered=true'))
    $answerPresentationEvidenceSatisfied = -not $diffMode -or (
        $answerTableObserved -and $answerHighlightObserved
    )
    $diffEvidenceSatisfied = -not $diffMode -or (
        $diffPathObserved -and $toolFoldObserved
    )
    $completionEvidenceSatisfied = -not $completionMode -or (
        $completionTaskSent -and $completionObserved -and $completionTextObserved -and
        $completionReasoningTailObserved -and $diffEvidenceSatisfied -and
        $answerPresentationEvidenceSatisfied
    )
    $commandsHelpObserved = -not $commandsMode -or ($decodedOutput -match '(?i)/login')
    $commandsEvidenceSatisfied = -not $commandsMode -or (
        $commandsHelpObserved -and $commandsLoginObserved -and $commandsModelsObserved -and $commandsSearchObserved -and
        $commandsEffortObserved -and $commandsAnswerObserved -and $commandsExitSent
    )
    $takeoverEvidenceSatisfied = -not $EscTakeover -or $sentEsc
    if ($session.BytesRead -eq 0) {
        throw 'ConPTY produced no output bytes'
    }
    if ($inputMode -and -not $inputEvidenceSatisfied) {
        $inputFailureKeylog = Join-Path $isolatedHome '.ridge\keylog.txt'
        throw "InputFixture did not prove raw-VT phased space/BS/DEL/TAB/Shift-Tab/CSI-OSC bracketed paste/physical LF/immediate-paste+CRLF routing (backend=$inputBackend reason=$inputBackendReason stage=$inputStage space=$inputSpaceObserved bs=$inputBackspaceObserved del=$inputDeleteObserved probe=$inputProbeObserved paste=$inputPasteObserved paste_transport=$inputPasteTransport tab=$inputTabObserved shift_tab=$inputShiftTabObserved shift_tab_transport=$inputShiftTabTransport enter_sent=$inputEnterSent enter_observed=$inputEnterObserved immediate_sent=$inputImmediateSent immediate_observed=$inputImmediateObserved immediate_busy=$inputImmediateBusyObserved; snapshot=$isolatedSnapshot; keylog=$inputFailureKeylog; raw_output=$rawOutputPath)"
    }
    if (-not $renderBudgetSatisfied) {
        throw "TUI render budget failed (frames=$renderFrameSequence samples=$renderSampleCount truncated=$renderSamplesTruncated p95_us=$renderP95Us/$RenderP95BudgetUs max_us=$renderMaxUs/$RenderMaxBudgetUs; snapshot=$isolatedSnapshot)"
    }
    if (-not $sentHelp -or -not $sentInterrupt -or -not $aliveAfterEnter) {
        throw 'ConPTY probe did not complete the Enter-then-interrupt sequence'
    }
    if (-not $session.HasExited) {
        throw "ridgecode did not exit after two Ctrl-C bytes within ${TimeoutMs}ms"
    }
    if (-not $inputMode -and -not $crosstermEventsObserved) {
        Write-Warning 'ConPTY byte pipe did not surface crossterm Windows INPUT_RECORD events; raw pipe boundary only.'
    }
    if ($snapshotRaw.Length -eq 0) {
        Write-Warning 'ConPTY probe did not produce an application frame snapshot; readable TUI frame remains unverified.'
    }
    $outputPrefixHex = (($rawOutput | Select-Object -First 64 | ForEach-Object { '{0:X2}' -f $_ }) -join '')
    $transportEvidenceSatisfied = if ($inputMode) {
        $inputBackend -eq 'raw-vt'
    } else {
        $crosstermEventsObserved
    }
    $overallEvidenceSatisfied = $startupAnimationEvidenceSatisfied -and
        $transportEvidenceSatisfied -and $nativeMousePathSatisfied -and
        $busyFixtureFrontObserved -and $queueEvidenceSatisfied -and
        $inspectEvidenceSatisfied -and $inspectQueueEvidenceSatisfied -and
        $reasoningEvidenceSatisfied -and $answerInspectEvidenceSatisfied -and
        $holdEvidenceSatisfied -and $inputEvidenceSatisfied -and
        $outputBudgetSatisfied -and $renderBudgetSatisfied -and
        $resizeEvidenceSatisfied -and $completionEvidenceSatisfied -and
        $commandsEvidenceSatisfied -and $takeoverEvidenceSatisfied
    $result = [pscustomobject]@{
        status = if ($overallEvidenceSatisfied) { 'passed' } else { 'partial' }
        binary = $binaryPath
        pid = $session.ProcessId
        columns = $Columns
        rows = $Rows
        input_bytes = $session.BytesWritten
        output_bytes = $session.BytesRead
        output_budget_bytes = $MaxOutputBytes
        output_budget_satisfied = $outputBudgetSatisfied
        output_prefix_hex = $outputPrefixHex
        startup_animation_evidence_satisfied = $startupAnimationEvidenceSatisfied
        startup_hide_index = $startupHideIndex
        startup_clear_index = $startupClearIndex
        startup_home_index = $startupHomeIndex
        startup_frame_home_index = $startupFrameHomeIndex
        startup_frame_next_home_index = $startupFrameNextHomeIndex
        startup_rgb_index = $startupRgbIndex
        startup_show_index = $startupShowIndex
        startup_log_index = $startupLogIndex
        startup_frame_line_breaks = $startupFrameLineBreaks
        startup_expected_frame_line_breaks = $startupExpectedFrameLineBreaks
        output_text_preview = if ($plain.Length -gt 1000) { $plain.Substring(0, 1000) } else { $plain }
        output_text_tail = if ($plain.Length -gt 1000) { $plain.Substring($plain.Length - 1000) } else { $plain }
        raw_output_path = if ($KeepDiagnostics) { $rawOutputPath } else { $null }
        workspace_path = if ($KeepDiagnostics) { $isolatedWorkspace } else { $null }
        output_has_ridge_marker = ($plain -match 'RIDGE|RidgeCode|ready')
        output_has_completion_reasoning = $completionReasoningObserved
        output_has_completion_reasoning_tail = $completionReasoningTailObserved
        output_has_completion_answer = $completionAnswerObserved
        mouse_capture_enable_observed = $mouseCaptureEnableObserved
        mouse_capture_disable_observed = $mouseCaptureDisableObserved
        native_mouse_path_satisfied = $nativeMousePathSatisfied
        snapshot_bytes = [Text.Encoding]::UTF8.GetByteCount($snapshotRaw)
        snapshot_render_us = $snapshotRenderUs
        render_frame_sequence = $renderFrameSequence
        render_sample_count = $renderSampleCount
        render_p95_us = $renderP95Us
        render_max_us = $renderMaxUs
        render_samples_truncated = $renderSamplesTruncated
        render_p95_budget_us = $RenderP95BudgetUs
        render_max_budget_us = $RenderMaxBudgetUs
        render_budget_satisfied = $renderBudgetSatisfied
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
        snapshot_input_probe_bytes = [Text.Encoding]::UTF8.GetByteCount($snapshotInputProbeRaw)
        snapshot_input_probe_json_valid = ($null -ne $snapshotInputProbeJson)
        snapshot_input_probe_buffer = if ($null -ne $snapshotInputProbeJson -and $null -ne $snapshotInputProbeJson.input) { $snapshotInputProbeJson.input.buffer } else { $null }
        snapshot_input_probe_cursor = if ($null -ne $snapshotInputProbeJson -and $null -ne $snapshotInputProbeJson.input) { $snapshotInputProbeJson.input.cursor } else { $null }
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
        hold_frame_observed = $holdFrameObserved
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
        diff_path_observed = $diffPathObserved
        diff_removed_observed = $diffRemovedObserved
        diff_added_observed = $diffAddedObserved
        diff_removed_tail_observed = $diffRemovedTailObserved
        diff_added_tail_observed = $diffAddedTailObserved
        tool_fold_observed = $toolFoldObserved
        answer_table_observed = $answerTableObserved
        answer_highlight_observed = $answerHighlightObserved
        diff_evidence_satisfied = $diffEvidenceSatisfied
        completion_evidence_satisfied = $completionEvidenceSatisfied
        commands_fixture_requested = $commandsMode
        commands_help_observed = $commandsHelpObserved
        commands_login_observed = $commandsLoginObserved
        commands_models_observed = $commandsModelsObserved
        commands_search_observed = $commandsSearchObserved
        commands_effort_observed = $commandsEffortObserved
        commands_answer_observed = $commandsAnswerObserved
        commands_evidence_satisfied = $commandsEvidenceSatisfied
        input_fixture_requested = $inputMode
        input_stage = $inputStage
        input_probe_sent = $sentInputProbe
        input_probe_observed = $inputProbeObserved
        input_space_observed = $inputSpaceObserved
        input_backspace_observed = $inputBackspaceObserved
        input_delete_observed = $inputDeleteObserved
        input_paste_sent = $inputPasteSent
        input_paste_observed = $inputPasteObserved
        input_paste_transport = $inputPasteTransport
        input_backend = $inputBackend
        input_backend_reason = $inputBackendReason
        input_raw_vt_transport_proven = ($inputMode -and $inputBackend -eq 'raw-vt')
        input_tab_observed = $inputTabObserved
        input_shift_tab_sent = $inputShiftTabSent
        input_shift_tab_observed = $inputShiftTabObserved
        input_shift_tab_transport = $inputShiftTabTransport
        input_enter_sent = $inputEnterSent
        input_enter_observed = $inputEnterObserved
        input_immediate_paste_enter_sent = $inputImmediateSent
        input_immediate_paste_enter_observed = $inputImmediateObserved
        input_immediate_busy_observed = $inputImmediateBusyObserved
        input_evidence_satisfied = $inputEvidenceSatisfied
        snapshot_path = if ($KeepDiagnostics) { $isolatedSnapshot } else { $null }
        trace = $trace
        trace_path = if ($KeepDiagnostics) { $isolatedTrace } else { $null }
        keylog_event_count = $keylogEvents.Count
        keylog_has_enter = $keylogHasEnter
        keylog_has_ctrl_c = $keylogHasCtrlC
        crossterm_events_observed = $crosstermEventsObserved
        transport_evidence_satisfied = $transportEvidenceSatisfied
        raw_enter_sent = $sentHelp
        busy_fixture_front_sent = $sentFront
        busy_fixture_tail_sent = $sentQueueTail
        busy_fixture_front_fallback_sent = $frontFallbackSent
        busy_fixture_front_transport = if (-not $BusyFixture) { 'not-applicable' } elseif ($frontFallbackSent) { 'csi-u→legacy-crlf' } else { 'csi-u' }
        busy_fixture_front_observed = $busyFixtureFrontObserved
        live_inspector_requested = [bool]$InspectLive
        reasoning_requested = [bool]$InspectReasoning
        reasoning_sent = $sentReasoning
        reasoning_observed = $reasoningObserved
        answer_inspect_requested = [bool]$InspectAnswer
        answer_inspect_sent = $sentAnswerInspect
        answer_archive_inspect_sent = $sentAnswerArchiveInspect
        answer_inspect_end_sent = $sentAnswerInspectEnd
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
        live_inspector_collapse_sent = $sentInspectCollapse
        live_inspector_collapsed_observed = $inspectCollapsedObserved
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
    }
    $result | ConvertTo-Json -Compress
    if (-not $overallEvidenceSatisfied -and -not $AllowPartial) {
        throw 'ConPTY probe completed with partial evidence; pass -AllowPartial only for diagnostics'
    }
} finally {
    if ($null -ne $session) { $session.Dispose() }
    if ($null -eq $previousConfig) {
        Remove-Item Env:RIDGECODE_CONFIG -ErrorAction SilentlyContinue
    } else {
        $env:RIDGECODE_CONFIG = $previousConfig
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
        Remove-ValidatedTempDirectory $isolatedWorkspace "ridgecode-pty-$runId-workspace"
        Remove-ValidatedTempDirectory $isolatedHome "ridgecode-pty-$runId-home"
    }
}
