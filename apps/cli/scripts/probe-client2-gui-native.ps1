param(
    [Parameter(Mandatory)][string]$BindingPath,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [switch]$OfflineAcceptance,
    [switch]$SourceCandidateStability,
    [switch]$ExistingKeys,
    [switch]$CoreConfigQuery,
    [ValidateSet('issue','resume','verified')][string]$RecoveryPhase
)
$ErrorActionPreference = 'Stop'
$report = [ordered]@{
    method = 'Windows UI Automation + OS SendInput Tab + desktop target-window capture'
    identity_verified = $false; accessibility_tree_read = $false; foreground_verified = $false
    native_tab_sent = $false; focus_changed_in_target_document = $false
    target_desktop_screenshot_saved = $false; status = 'failed'
    full_gui_acceptance = 'not-evaluated'; g1_passed = $false
}

. (Join-Path $PSScriptRoot 'probe-client2-document.ps1')
. (Join-Path $PSScriptRoot 'probe-client2-offline-controls.ps1')
. (Join-Path $PSScriptRoot 'probe-client2-account-controls.ps1')
. (Join-Path $PSScriptRoot 'probe-client2-runtime-recovery.ps1')

try {
    if ($PSVersionTable.PSVersion.Major -ne 5 -or $PSVersionTable.PSVersion.Minor -ne 1 -or -not [Environment]::Is64BitProcess) { throw 'Require the system 64-bit Windows PowerShell 5.1 helper' }
    $binding = Get-Content -Raw -LiteralPath $BindingPath | ConvertFrom-Json
    if ($SourceCandidateStability) {
        if (-not $OfflineAcceptance -or $binding.fixture_mode -cne 'source-client2-ui-candidate' -or
            $binding.source_commit -cnotmatch '\A[0-9a-f]{40}\z') { throw 'Candidate stability requires an explicit source binding and offline subset' }
        $report.fixture_mode='source-client2-ui-candidate'
    } elseif ($binding.fixture_mode -eq 'source-client2-ui-candidate') { throw 'Candidate source binding requires stability verification' }
    if ($binding.full_gui_acceptance -ne 'not-evaluated' -or $binding.g1_passed -ne $false -or
        (Get-Process -Id $PID).SessionId -ne $binding.session_id) { throw 'Invalid isolated native binding/session' }
    if ((Get-FileHash -LiteralPath $binding.exe -Algorithm SHA256).Hash.ToLowerInvariant() -ne $binding.exe_sha256) { throw 'Original EXE bytes changed' }
    Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes,System.Drawing,System.Windows.Forms
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class RespireNativeInput {
    [StructLayout(LayoutKind.Sequential)] struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] struct POINT { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx,dy; public uint data,flags,time; public UIntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort vk,scan; public uint flags,time; public UIntPtr extra; }
    [StructLayout(LayoutKind.Explicit)] struct UNION { [FieldOffset(0)] public MOUSEINPUT mouse; [FieldOffset(0)] public KEYBDINPUT keyboard; }
    [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public UNION input; }
    [DllImport("user32.dll", SetLastError=true)] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", SetLastError=true)] static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll", SetLastError=true)] static extern bool GetClientRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll", SetLastError=true)] static extern bool ClientToScreen(IntPtr hwnd, ref POINT point);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd,int attribute,out int value,int size);
    [DllImport("user32.dll", EntryPoint="GetForegroundWindow")] public static extern IntPtr Foreground();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll", SetLastError=true)] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr hwnd, int command);
    [DllImport("user32.dll")] static extern bool IsIconic(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool IsZoomed(IntPtr hwnd);
    [DllImport("user32.dll", SetLastError=true)] static extern bool SetWindowPos(IntPtr hwnd,IntPtr after,int x,int y,int width,int height,uint flags);
    [DllImport("user32.dll", SetLastError=true)] static extern uint SendInput(uint count, INPUT[] events, int size);
    public static object Window(IntPtr hwnd) {
        uint pid; RECT rect,client; int cloaked;
        if (GetWindowThreadProcessId(hwnd,out pid)==0 || !GetWindowRect(hwnd,out rect)) throw new Win32Exception();
        if(!GetClientRect(hwnd,out client)) throw new Win32Exception();
        var origin=new POINT { X=client.Left, Y=client.Top }; var end=new POINT { X=client.Right, Y=client.Bottom };
        if(!ClientToScreen(hwnd,ref origin) || !ClientToScreen(hwnd,ref end)) throw new Win32Exception();
        int hr=DwmGetWindowAttribute(hwnd,14,out cloaked,sizeof(int)); if(hr!=0) Marshal.ThrowExceptionForHR(hr);
        return new { pid=pid, visible=IsWindowVisible(hwnd), cloaked=cloaked, bounds=new[]{rect.Left,rect.Top,rect.Right,rect.Bottom}, client_bounds=new[]{origin.X,origin.Y,end.X,end.Y} };
    }
    public static uint Key(IntPtr target,ushort key) {
        if(Foreground()!=target) throw new InvalidOperationException("Foreground changed immediately before real keyboard input");
        var events=new INPUT[2]; events[0].type=1; events[0].input.keyboard.vk=key;
        events[1].type=1; events[1].input.keyboard.vk=key; events[1].input.keyboard.flags=2;
        return SendInput(2,events,Marshal.SizeOf(typeof(INPUT)));
    }
    public static uint Text(IntPtr target,string text) {
        if(Foreground()!=target) throw new InvalidOperationException("Foreground changed immediately before real text input");
        var events=new INPUT[text.Length*2];
        for(int i=0;i<text.Length;i++) {
            events[i*2].type=1; events[i*2].input.keyboard.scan=text[i]; events[i*2].input.keyboard.flags=4;
            events[i*2+1].type=1; events[i*2+1].input.keyboard.scan=text[i]; events[i*2+1].input.keyboard.flags=6;
        }
        return SendInput((uint)events.Length,events,Marshal.SizeOf(typeof(INPUT)));
    }
    public static uint Tab(IntPtr target) {
        if(Foreground()!=target) throw new InvalidOperationException("Foreground changed immediately before SendInput");
        var events=new INPUT[2]; events[0].type=1; events[0].input.keyboard.vk=9;
        events[1].type=1; events[1].input.keyboard.vk=9; events[1].input.keyboard.flags=2;
        return SendInput(2,events,Marshal.SizeOf(typeof(INPUT)));
    }
    public static void RestoreAndPosition(IntPtr target,uint expectedPid,int x,int y,int width,int height) {
        uint pid;
        if(GetWindowThreadProcessId(target,out pid)==0 || pid!=expectedPid) throw new InvalidOperationException("Target HWND owner changed before restore");
        ShowWindow(target,9); // SW_RESTORE. Return value is prior visibility, not success.
        if(GetWindowThreadProcessId(target,out pid)==0 || pid!=expectedPid) throw new InvalidOperationException("Target HWND owner changed before positioning");
        if(!SetWindowPos(target,IntPtr.Zero,x,y,width,height,0x0014)) throw new Win32Exception(); // NOZORDER | NOACTIVATE
        if(IsIconic(target) || IsZoomed(target)) throw new InvalidOperationException("Target did not restore to a normal window");
    }
}
'@
    if ([RespireNativeInput]::SetThreadDpiAwarenessContext([IntPtr](-4)) -eq [IntPtr]::Zero) { throw 'Cannot establish per-monitor-v2 thread coordinates for target desktop capture' }
    $report.capture_coordinate_context = 'per-monitor-v2 helper thread; ends with helper process'
    if ($RecoveryPhase -in @('resume','verified')) {
        $script:recoveryMaterialVisible = $true
        $report.initial_guard = Assert-Target
        Prepare-TargetWindow
        Focus-Target
        Wait-TargetDocument
    } else { Invoke-NativeGuiProbe }
    if ($CoreConfigQuery) { Invoke-CoreQueryAcceptance }
    elseif ($RecoveryPhase) { Invoke-RecoveryMaterialAcceptance }
    elseif ($OfflineAcceptance) {
        if ($ExistingKeys) { Invoke-ExistingKeysAcceptance } else { Invoke-OfflineGuiAcceptance }
    }
} catch {
    $report.error = $_.Exception.Message
    if ($CoreConfigQuery -and $report.core_query) {
        $report.core_query.failure_script_line = $_.InvocationInfo.ScriptLineNumber
        try { $report.core_query.failure_counts = Read-CoreQueryControl -FailureEvidence }
        catch { $report.core_query.failure_counts_error = 'Could not read real forwarding counters' }
    }
}
finally {
    $json = $report | ConvertTo-Json -Depth 12
    [IO.File]::WriteAllText((Join-Path $OutputDirectory 'native-gui.json'), $json + "`n", [Text.UTF8Encoding]::new($false))
}
if ($report.status -ne 'native-connectivity-passed' -or
    ($CoreConfigQuery -and $report.core_query.status -ne ($binding.core_query.phase + '-passed')) -or
    ($RecoveryPhase -and $report.recovery.status -ne 'recovery-subset-passed') -or
    ($OfflineAcceptance -and -not $RecoveryPhase -and $report.offline.status -ne 'offline-subset-passed')) { exit 1 }
exit 0
