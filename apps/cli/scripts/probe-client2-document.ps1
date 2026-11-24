function Assert-Target([bool]$RequireForeground = $false) {
    $owner = Get-Process -Id $binding.desktop_pid -ErrorAction Stop
    if ($owner.StartTime.ToUniversalTime().Ticks -ne $binding.desktop_started_ticks -or
        $owner.SessionId -ne $binding.session_id -or
        -not [string]::Equals($owner.Path, $binding.exe, [StringComparison]::OrdinalIgnoreCase)) { throw 'Original EXE identity/session changed' }
    $all = @(Get-CimInstance Win32_Process)
    $ids = [Collections.Generic.HashSet[int]]::new(); [void]$ids.Add([int]$binding.desktop_pid)
    do {
        $added = $false
        foreach ($item in $all) { if ($ids.Contains([int]$item.ParentProcessId) -and $ids.Add([int]$item.ProcessId)) { $added = $true } }
    } while ($added)
    $browser = @($all | Where-Object { $_.ProcessId -eq $binding.browser_pid })
    $browserProcess = Get-Process -Id $binding.browser_pid -ErrorAction Stop
    $profileArg = '--user-data-dir=(?:"' + [regex]::Escape($binding.webview_profile) + '"|' + [regex]::Escape($binding.webview_profile) + '(?=\s|$))'
    if ($browser.Count -ne 1 -or -not $ids.Contains([int]$binding.browser_pid) -or
        $browserProcess.StartTime.ToUniversalTime().Ticks -ne $binding.browser_started_ticks -or
        $browser[0].SessionId -ne $binding.session_id -or
        [IO.Path]::GetFileName($browser[0].ExecutablePath) -ne 'msedgewebview2.exe' -or
        $browser[0].CommandLine -notmatch $profileArg) { throw 'Original WebView descendant/profile/birth changed' }
    $window = [RespireNativeInput]::Window([IntPtr][long]$binding.hwnd)
    if ($window.pid -ne $binding.desktop_pid -or -not $window.visible -or $window.cloaked -ne 0 -or
        $window.bounds[2] -le $window.bounds[0] -or $window.bounds[3] -le $window.bounds[1] -or
        $window.client_bounds[2] -le $window.client_bounds[0] -or $window.client_bounds[3] -le $window.client_bounds[1]) { throw 'Target HWND is no longer a visible original window/client area' }
    if ($RequireForeground -and [RespireNativeInput]::Foreground().ToInt64() -ne $binding.hwnd) { throw 'Foreground is not the bound target HWND; no input/capture permitted' }
    $script:ownedIds = $ids
    $script:candidateProcessSnapshot = $all # Existing successful guard snapshot; diagnostic only.
    return @{ bounds = @($window.bounds); client_bounds = @($window.client_bounds); foreground = [RespireNativeInput]::Foreground().ToInt64() }
}

function Element-Info($Element) {
    $current = $Element.Current
    # UIA may expose an unnamed transient node; diagnostic names are optional.
    $name = if ($current.IsPassword) { '' } else { [string]$current.Name }
    return @{ runtime_id = ($Element.GetRuntimeId() -join '.'); process_id = $current.ProcessId
        control_type = $current.ControlType.ProgrammaticName; automation_id = $current.AutomationId
        class_name = $current.ClassName; name = $name.Substring(0, [Math]::Min(128, $name.Length))
        keyboard_focusable = $current.IsKeyboardFocusable; offscreen = $current.IsOffscreen }
}

function Get-DocumentUnavailableException([Exception]$Exception) {
    # Only known method-call wrappers, never arbitrary InnerException chains,
    # message text, HRESULT guesses, or provider/COM errors.
    $current = $Exception
    for ($depth=0; $null -ne $current -and $depth -le 4; $depth++) {
        if ($current.GetType() -eq [Windows.Automation.ElementNotAvailableException]) { return $current }
        if ($current.GetType() -ne [Management.Automation.MethodInvocationException] -and
            $current.GetType() -ne [Reflection.TargetInvocationException]) { return $null }
        $current = $current.InnerException
    }
    return $null
}
function New-DocumentReadinessClock { return [Diagnostics.Stopwatch]::StartNew() }
function Read-DocumentProperty($Element, $Property, [switch]$Readiness) {
    try { $value = $Element.GetCurrentPropertyValue($Property, $true) } catch {
        $unavailable = Get-DocumentUnavailableException $_.Exception
        if ($Readiness -and $null -ne $unavailable) {
            $report.document_pid_diagnostic = 'element_unavailable'
            return $unavailable
        }
        throw 'UIA document property method failed'
    }
    # Envelope preserves null, arrays and the exact provider type for validation.
    return @{value=$value}
}
function Read-DocumentRuntimeId($Element, [switch]$Readiness) {
    try { $value = @($Element.GetRuntimeId()) } catch {
        $unavailable = Get-DocumentUnavailableException $_.Exception
        if ($Readiness -and $null -ne $unavailable) {
            $report.document_pid_diagnostic = 'element_unavailable'
            return $unavailable
        }
        throw 'UIA document runtime identity method failed'
    }
    return @{value=$value}
}
function Assert-DocumentInspectionPid($Value, [int]$ExpectedPid, $AdmittedIds, $LiveIdentities) {
    if ($Value -isnot [int] -or $Value -le 0 -or $Value -ne $ExpectedPid -or -not $AdmittedIds.Contains($Value)) {
        throw 'UIA node process changed during inspection'
    }
    if ($null -ne $LiveIdentities) { Assert-DocumentNodeIdentity $Value $LiveIdentities }
}
function Read-DocumentNodeInfo($Element, [int]$ExpectedPid, $AdmittedIds, $LiveIdentities, [switch]$Readiness) {
    # Each provider method has its own narrow catch. All validation is outside
    # those catches and can never become a resample signal.
    $sample = Read-DocumentProperty $Element ([Windows.Automation.AutomationElement]::ProcessIdProperty) -Readiness:$Readiness
    if ($sample -is [Windows.Automation.ElementNotAvailableException]) { return $sample }
    Assert-DocumentInspectionPid $sample.value $ExpectedPid $AdmittedIds $LiveIdentities
    # Startup readiness never needs text or descriptive metadata. Do not read
    # Name, AutomationId, ClassName or IsPassword, even for diagnostics.
    $info = @{process_id=$ExpectedPid}
    $sample = Read-DocumentRuntimeId $Element -Readiness:$Readiness
    if ($sample -is [Windows.Automation.ElementNotAvailableException]) { return $sample }
    if ($sample.value.Count -eq 0 -or @($sample.value | Where-Object { $_ -isnot [int] }).Count -gt 0) { throw 'UIA node runtime identity is unknown' }
    $info.runtime_id = $sample.value -join '.'
    foreach ($field in @(
        @{key='control_type';property=[Windows.Automation.AutomationElement]::ControlTypeProperty;type=[Windows.Automation.ControlType]},
        @{key='keyboard_focusable';property=[Windows.Automation.AutomationElement]::IsKeyboardFocusableProperty;type=[bool]},
        @{key='offscreen';property=[Windows.Automation.AutomationElement]::IsOffscreenProperty;type=[bool]}
    )) {
        $sample = Read-DocumentProperty $Element $field.property -Readiness:$Readiness
        if ($sample -is [Windows.Automation.ElementNotAvailableException]) { return $sample }
        if ($null -eq $sample.value -or -not $field.type.IsInstanceOfType($sample.value)) {
            if (-not $Readiness) { throw 'UIA node diagnostic property type is unknown' }
            # Unknown is not false and cannot supply a readiness witness. Other
            # nodes may prove readiness, but every node still needs full identity.
            $info[$field.key] = $null
            continue
        }
        if ($field.key -eq 'control_type') { $info[$field.key]=$sample.value.ProgrammaticName }
        else { $info[$field.key]=$sample.value }
    }
    $sample = Read-DocumentProperty $Element ([Windows.Automation.AutomationElement]::ProcessIdProperty) -Readiness:$Readiness
    if ($sample -is [Windows.Automation.ElementNotAvailableException]) { return $sample }
    Assert-DocumentInspectionPid $sample.value $ExpectedPid $AdmittedIds $LiveIdentities
    return $info
}
function Read-TargetTree([switch]$ReportUnknownProcess, [Collections.Generic.HashSet[int]]$AdmittedIds = $null, $LiveIdentities = $null) {
    if ($null -eq $AdmittedIds) { $AdmittedIds = $ownedIds }
    $window = Get-OfflineReplacementRoot
    $walker = Get-OfflineReplacementWalker
    $queue = [Collections.Generic.Queue[object]]::new(); $queue.Enqueue(@{element=$window; depth=0})
    $rows = [Collections.Generic.List[object]]::new(); $clock = [Diagnostics.Stopwatch]::StartNew()
    $nodeLimited = $false; $depthLimited = $false; $timeLimited = $false
    while ($queue.Count -gt 0 -and $rows.Count -lt 256 -and $clock.Elapsed.TotalSeconds -lt 5) {
        $next = $queue.Dequeue()
        # Never inspect diagnostic names/values of a node whose process is unproven.
        # Explicit method invocation preserves provider exceptions rather than
        # relying on PowerShell's property-getter exception handling.
        try { $rawPid = $next.element.GetCurrentPropertyValue([Windows.Automation.AutomationElement]::ProcessIdProperty, $true) } catch {
            $unavailable = Get-DocumentUnavailableException $_.Exception
            if ($null -ne $unavailable -and $ReportUnknownProcess) {
                $report.document_pid_diagnostic = 'element_unavailable'
                # Private typed signal; no partial rows or exception details enter
                # the report. Only Wait-TargetDocument handles this exact type.
                return $unavailable
            }
            $report.document_pid_diagnostic = 'unavailable'
            throw
        }
        if ($null -eq $rawPid) {
            $report.document_pid_diagnostic = 'unavailable'
            throw 'UIA node belongs to an unbound process'
        }
        if ($rawPid -isnot [int]) {
            $report.document_pid_diagnostic = 'invalid_type'
            throw 'UIA node belongs to an unbound process'
        }
        $nodePid = $rawPid
        if ($nodePid -le 0) {
            $report.document_pid_diagnostic = 'nonpositive'
            throw 'UIA node belongs to an unbound process'
        }
        if (-not $AdmittedIds.Contains($nodePid)) {
            $report.document_pid_diagnostic = 'unbound_positive'
            # Private typed result: an Int32 means discard this entire traversal.
            if ($ReportUnknownProcess -and $nodePid -gt 0) { return [int]$nodePid }
            throw 'UIA node belongs to an unbound process'
        }
        if ($null -ne $LiveIdentities) { Assert-DocumentNodeIdentity $nodePid $LiveIdentities }
        $info = Read-DocumentNodeInfo $next.element $nodePid $AdmittedIds $LiveIdentities -Readiness:$ReportUnknownProcess
        if ($info -is [Windows.Automation.ElementNotAvailableException]) { return $info }
        $info.depth = $next.depth
        $rows.Add($info)
        if ($next.depth -lt 32) {
            $child = $walker.GetFirstChild($next.element)
            while ($child -and $queue.Count + $rows.Count -lt 256 -and $clock.Elapsed.TotalSeconds -lt 5) {
                $queue.Enqueue(@{element=$child; depth=$next.depth+1}); $child = $walker.GetNextSibling($child)
            }
            if ($child) {
                if ($queue.Count + $rows.Count -ge 256) { $nodeLimited = $true }
                if ($clock.Elapsed.TotalSeconds -ge 5) { $timeLimited = $true }
            }
        } elseif ($walker.GetFirstChild($next.element)) { $depthLimited = $true }
    }
    if ($queue.Count -gt 0) {
        if ($rows.Count -ge 256) { $nodeLimited = $true }
        if ($clock.Elapsed.TotalSeconds -ge 5) { $timeLimited = $true }
    }
    $reasons = @()
    if ($timeLimited) { $reasons += 'time-budget' }
    if ($nodeLimited) { $reasons += 'node-limit' }
    if ($depthLimited) { $reasons += 'depth-limit' }
    # Complete traversal proves enumeration, not complete property knowledge.
    # Unknown observations remain explicit and cannot provide positive witnesses.
    return @{ nodes = @($rows.ToArray()); bounded = $true; truncation_reasons = $reasons
        traversal_complete = $reasons.Count -eq 0; elapsed_ms = $clock.ElapsedMilliseconds
        max_nodes = 256; max_depth = 32; time_budget_seconds = 5
        document_count = @($rows | Where-Object { $_.control_type -ceq 'ControlType.Document' -and $_.offscreen -is [bool] -and $_.offscreen -eq $false }).Count
        focusable_count = @($rows | Where-Object { $_.keyboard_focusable -is [bool] -and $_.keyboard_focusable -eq $true -and $_.offscreen -is [bool] -and $_.offscreen -eq $false }).Count
        unknown_control_type_count = @($rows | Where-Object { $null -eq $_.control_type }).Count
        unknown_keyboard_focusable_count = @($rows | Where-Object { $null -eq $_.keyboard_focusable }).Count
        unknown_offscreen_count = @($rows | Where-Object { $null -eq $_.offscreen }).Count }
}

function Assert-DocumentProcessHandle($Process) {
    try {
        $handle = $Process.SafeHandle
        if ($null -eq $handle) { throw 'Document process handle acquisition failed' }
        $invalid = $handle.IsInvalid
        $closed = $handle.IsClosed
        if ($invalid -isnot [bool] -or $closed -isnot [bool] -or $invalid -or $closed) {
            throw 'Document process handle acquisition failed'
        }
    } catch { throw 'Document process handle acquisition failed' }
}
function Assert-DocumentNodeIdentity([int]$NodePid, $LiveIdentities) {
    if (-not $LiveIdentities.ContainsKey($NodePid)) { throw 'Document node has no held process identity' }
    $identity = $LiveIdentities[$NodePid]
    $process = $identity.process
    Assert-DocumentProcessHandle $process
    if ($process.HasExited -or $process.StartTime.ToUniversalTime().Ticks -ne $identity.birth -or
        $process.SessionId -ne $identity.session -or
        -not [string]::Equals($process.Path,$identity.image,[StringComparison]::OrdinalIgnoreCase)) {
        throw 'Document node process identity changed after admission'
    }
}
function Read-ProvenWebViewChain([int]$NodePid, [Diagnostics.Stopwatch]$BudgetClock, [double]$Seconds, $RetainedHandles = $null) {
    $transferred = $false
    $held = [Collections.Generic.List[object]]::new()
    try {
    $browser = Get-Process -Id $binding.browser_pid -ErrorAction Stop
    $held.Add($browser)
    Assert-DocumentProcessHandle $browser
    if ($browser.HasExited -or $browser.StartTime.ToUniversalTime().Ticks -ne $binding.browser_started_ticks -or
        $browser.SessionId -ne $binding.session_id -or
        [IO.Path]::GetFileName($browser.Path) -ne 'msedgewebview2.exe') { throw 'WebView chain browser identity changed' }
    $chain = [Collections.Generic.List[object]]::new()
    $seen = [Collections.Generic.HashSet[int]]::new()
    $cursor = $NodePid
      for ($depth=0; $depth -lt 32; $depth++) {
        if ($BudgetClock.Elapsed.TotalSeconds -ge $Seconds) { throw 'Document readiness budget exhausted during descendant verification' }
        if (-not $seen.Add($cursor)) { throw 'WebView process chain contains a cycle' }
        $live = Get-Process -Id $cursor -ErrorAction Stop
        $held.Add($live)
        Assert-DocumentProcessHandle $live
        if ($live.HasExited) { throw 'WebView chain process exited during verification' }
        $matches = @(Get-CimInstance Win32_Process -Filter "ProcessId=$cursor")
        if ($matches.Count -ne 1 -or $matches[0].ProcessId -ne $cursor) { throw 'WebView process chain is missing or ambiguous' }
        $item = $matches[0]
        # Keep the live process handle across CIM sampling. Its uninterrupted
        # lifetime binds PID to this sample without equating API time precision.
        if ($live.HasExited) { throw 'WebView chain process exited during verification' }
        if ($item.CreationDate -isnot [datetime] -or
            $item.SessionId -ne $binding.session_id -or $live.SessionId -ne $binding.session_id -or
            -not [string]::Equals($item.ExecutablePath,$browser.Path,[StringComparison]::OrdinalIgnoreCase) -or
            -not [string]::Equals($live.Path,$browser.Path,[StringComparison]::OrdinalIgnoreCase)) { throw 'WebView process chain identity is unproven' }
        $birth = $live.StartTime.ToUniversalTime().Ticks
        if ($chain.Count -gt 0 -and $birth -gt $chain[$chain.Count-1].birth) { throw 'WebView process parent is younger than its child' }
        $chain.Add(@{pid=$cursor; parent=[int]$item.ParentProcessId; birth=$birth
            cim_birth=$item.CreationDate.ToUniversalTime().Ticks; image=$live.Path; session=$live.SessionId; process=$live})
        if ($cursor -eq $binding.browser_pid) {
            if ($birth -ne $binding.browser_started_ticks -or $chain.Count -lt 2) { throw 'Unknown node is not a new WebView descendant' }
            foreach ($process in $held) { if ($process.HasExited) { throw 'WebView chain process exited during verification' } }
            if ($null -ne $RetainedHandles) {
                foreach ($process in $held) { $RetainedHandles.Add($process) }
                $transferred = $true
            }
            return $chain.ToArray()
        }
        $cursor = [int]$item.ParentProcessId
      }
      throw 'WebView process chain exceeds its bound'
    } finally { if (-not $transferred) { foreach ($process in $held) { $process.Dispose() } } }
}
function Confirm-NewWebViewDescendant([int]$NodePid, [Diagnostics.Stopwatch]$BudgetClock, [double]$Seconds, $RetainedHandles) {
    $null = Assert-Target $true
    $before = @(Read-ProvenWebViewChain $NodePid $BudgetClock $Seconds)
    $null = Assert-Target $true
    $after = @(Read-ProvenWebViewChain $NodePid $BudgetClock $Seconds $RetainedHandles)
    if ($before.Count -ne $after.Count) { throw 'WebView process chain changed during verification' }
    for ($i=0; $i -lt $before.Count; $i++) {
        foreach ($field in @('pid','parent','birth','cim_birth','image','session')) {
            if ($before[$i][$field] -cne $after[$i][$field]) { throw 'WebView process chain changed during verification' }
        }
    }
    $null = Assert-Target $true
    return $after
}

function Wait-TargetDocument([double]$Seconds = 15) {
    $clock = New-DocumentReadinessClock
    $report.document_discarded_samples = 0
    $attempts = [Collections.Generic.List[object]]::new()
    $refreshed = $false
    $admitted = $null
    $held = [Collections.Generic.List[object]]::new()
    $identities = @{}
    try {
    while ($clock.Elapsed.TotalSeconds -lt $Seconds) {
        $report.document_readiness_phase = 'target_guard'
        $null = Assert-Target $true
        if ($null -eq $admitted) {
            $admitted = [Collections.Generic.HashSet[int]]::new()
            foreach ($knownPid in $ownedIds) {
                if ($clock.Elapsed.TotalSeconds -ge $Seconds) { throw 'Document readiness budget exhausted during identity acquisition' }
                $process = Get-Process -Id $knownPid -ErrorAction Stop
                $held.Add($process)
                Assert-DocumentProcessHandle $process
                $identities[[int]$knownPid] = @{process=$process; birth=$process.StartTime.ToUniversalTime().Ticks
                    session=$process.SessionId; image=$process.Path}
                Assert-DocumentNodeIdentity ([int]$knownPid) $identities
                [void]$admitted.Add([int]$knownPid)
            }
            # Sample ancestry while these exact process lifetimes remain held.
            $null = Assert-Target $true
            foreach ($knownPid in $admitted) {
                if (-not $ownedIds.Contains($knownPid)) { throw 'Initial document process ancestry changed' }
                Assert-DocumentNodeIdentity $knownPid $identities
            }
        }
        foreach ($knownPid in $admitted) {
            if (-not $ownedIds.Contains($knownPid)) { throw 'Document process ancestry changed before sampling' }
            Assert-DocumentNodeIdentity $knownPid $identities
        }
        $report.document_readiness_phase = 'read_tree'
        if ($clock.Elapsed.TotalSeconds -ge $Seconds) { break }
        $tree = Read-TargetTree -ReportUnknownProcess -AdmittedIds $admitted -LiveIdentities $identities
        if ($null -ne $tree -and $tree.GetType() -eq [Windows.Automation.ElementNotAvailableException]) {
            $report.document_discarded_samples++
            if ($clock.Elapsed.TotalSeconds -ge $Seconds) { break }
            $report.document_readiness_phase = 'discard_unavailable_sample'
            Pause-Probe
            # Keep held lifetimes, admitted IDs, proof allowance and clock intact.
            continue
        }
        if ($tree -is [int]) {
            if ($refreshed) { throw 'UIA node belongs to an unbound process after one verified refresh' }
            if ($clock.Elapsed.TotalSeconds -ge $Seconds) { break }
            $refreshed = $true
            $report.document_readiness_phase = 'verify_new_descendant'
            $proven = @(Confirm-NewWebViewDescendant $tree $clock $Seconds $held)
            foreach ($identity in $proven) {
                $identities[[int]$identity.pid] = $identity
                [void]$admitted.Add([int]$identity.pid)
            }
            $report.document_descendant_refresh = 'verified_discard_and_rescan'
            # No partial rows survive. The next iteration revalidates and starts at HWND.
            continue
        }
        if ($clock.Elapsed.TotalSeconds -ge $Seconds) { break }
        foreach ($knownPid in $admitted) { Assert-DocumentNodeIdentity $knownPid $identities }
        if ($clock.Elapsed.TotalSeconds -ge $Seconds) { break }
        $report.accessibility_tree = $tree
        $report.document_readiness_phase = 'record_tree'
        $attempts.Add(@{ document_count=$tree.document_count; focusable_count=$tree.focusable_count
            unknown_control_type_count=$tree.unknown_control_type_count
            unknown_keyboard_focusable_count=$tree.unknown_keyboard_focusable_count; unknown_offscreen_count=$tree.unknown_offscreen_count
            traversal_complete=$tree.traversal_complete; truncation_reasons=$tree.truncation_reasons; elapsed_ms=$tree.elapsed_ms })
        $report.document_readiness = @{ attempts=@($attempts.ToArray()); budget_seconds=$Seconds
            elapsed_ms=$clock.ElapsedMilliseconds; ready=$false }
        if ($tree.traversal_complete -eq $true -and @($tree.truncation_reasons).Count -eq 0 -and
            $tree.document_count -ge 1 -and $tree.focusable_count -ge 1) {
            $report.document_readiness.ready = $true
            $report.document_readiness_phase = 'ready'
            return
        }
        $report.document_readiness_phase = 'wait_document'
        Pause-Probe
    }
    $report.document_readiness_phase = 'budget_exhausted'
    throw 'Original WebView UIA Document/focusable controls not observed within readiness budget; inspect traversal truncation reasons'
    } finally { foreach ($process in $held) { $process.Dispose() } }
}

function Read-TargetFocus {
    $element = [Windows.Automation.AutomationElement]::FocusedElement
    if (-not $element) { throw 'UIA has no focused element' }
    $info = Element-Info $element
    $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$binding.hwnd)
    $rootId = $window.GetRuntimeId() -join '.'
    $info.belongs_to_target = $false; $info.in_document = $false
    for ($depth=0; $element -and $depth -lt 32; $depth++) {
        if (-not $ownedIds.Contains([int]$element.Current.ProcessId)) { break }
        if ($element.Current.ControlType -eq [Windows.Automation.ControlType]::Document) { $info.in_document = $true }
        if (($element.GetRuntimeId() -join '.') -eq $rootId) { $info.belongs_to_target = $true; break }
        $element = [Windows.Automation.TreeWalker]::RawViewWalker.GetParent($element)
    }
    return $info
}

function Focus-Target {
    if ([RespireNativeInput]::Foreground().ToInt64() -ne $binding.hwnd -and
        -not [RespireNativeInput]::SetForegroundWindow([IntPtr][long]$binding.hwnd)) { throw 'SetForegroundWindow was denied; no privilege/focus-policy changes attempted' }
}
function Send-TargetTab { return [RespireNativeInput]::Tab([IntPtr][long]$binding.hwnd) }
function Pause-Probe { Start-Sleep -Milliseconds 100 }

function Read-DesktopGeometry($Guard) {
    $virtual = [Windows.Forms.SystemInformation]::VirtualScreen
    $work = [Windows.Forms.Screen]::FromHandle([IntPtr][long]$binding.hwnd).WorkingArea
    $dpi = [RespireNativeInput]::GetDpiForWindow([IntPtr][long]$binding.hwnd)
    if ($dpi -eq 0) { throw 'Cannot read bound window DPI' }
    return @{ window_bounds=$Guard.bounds; client_bounds=$Guard.client_bounds; dpi=$dpi
        virtual_screen=@($virtual.Left,$virtual.Top,$virtual.Right,$virtual.Bottom)
        working_area=@($work.Left,$work.Top,$work.Right,$work.Bottom) }
}
function Move-TargetWindow([int[]]$Bounds) {
    [RespireNativeInput]::RestoreAndPosition([IntPtr][long]$binding.hwnd, [uint32]$binding.desktop_pid,
        $Bounds[0],$Bounds[1],$Bounds[2]-$Bounds[0],$Bounds[3]-$Bounds[1])
}
function Prepare-TargetWindow {
    $before = Assert-Target
    $geometry = Read-DesktopGeometry $before
    $report.window_placement_before = $geometry
    $work = $geometry.working_area
    $width = [Math]::Min($before.bounds[2]-$before.bounds[0],$work[2]-$work[0]-16)
    $height = [Math]::Min($before.bounds[3]-$before.bounds[1],$work[3]-$work[1]-16)
    # Fixed Wails WM_GETMINMAXINFO scales the original 800x600 minimum track size by window DPI.
    $minWidth = [Math]::Ceiling(800*$geometry.dpi/96.0)
    $minHeight = [Math]::Ceiling(600*$geometry.dpi/96.0)
    if ($width -lt $minWidth -or $height -lt $minHeight) { throw 'Hosted working area cannot fit the original minimum window size' }
    $requested = @(($work[0]+8),($work[1]+8),($work[0]+8+$width),($work[1]+8+$height))
    $report.window_placement_request = @{bounds=$requested; original_min_window_dips=@(800,600)}
    $null = Assert-Target
    Move-TargetWindow $requested
    $after = Assert-Target
    $placed = Read-DesktopGeometry $after
    $report.window_placement_after = $placed
    $client = $after.client_bounds
    if (($placed.working_area -join ',') -ne ($work -join ',') -or
        $after.bounds[0] -lt $work[0] -or $after.bounds[1] -lt $work[1] -or $after.bounds[2] -gt $work[2] -or $after.bounds[3] -gt $work[3] -or
        $client[0] -lt $work[0] -or $client[1] -lt $work[1] -or $client[2] -gt $work[2] -or $client[3] -gt $work[3] -or
        $after.bounds[2]-$after.bounds[0] -lt [Math]::Ceiling(800*$placed.dpi/96.0) -or
        $after.bounds[3]-$after.bounds[1] -lt [Math]::Ceiling(600*$placed.dpi/96.0)) { throw 'Restored target did not retain its original minimum window size with client area inside the working area' }
}

function Capture-Target([int[]]$ExpectedBounds) {
    if ($script:recoveryMaterialVisible) { throw 'Capture is forbidden until recovery material is acknowledged and dismissed' }
    $before = Assert-Target $true
    $report.capture_geometry_before = Read-DesktopGeometry $before
    if (($before.client_bounds -join ',') -ne ($ExpectedBounds -join ',')) { throw 'Target client bounds changed before desktop capture' }
    $rectangle = [Drawing.Rectangle]::new($ExpectedBounds[0], $ExpectedBounds[1], $ExpectedBounds[2]-$ExpectedBounds[0], $ExpectedBounds[3]-$ExpectedBounds[1])
    if (-not [Windows.Forms.SystemInformation]::VirtualScreen.Contains($rectangle)) { throw 'Target client area extends outside the visible desktop' }
    $bitmap = [Drawing.Bitmap]::new($rectangle.Width, $rectangle.Height)
    try {
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try { $graphics.CopyFromScreen($rectangle.Location, [Drawing.Point]::Empty, $rectangle.Size, [Drawing.CopyPixelOperation]::SourceCopy) }
        finally { $graphics.Dispose() }
        $after = Assert-Target $true
        $report.capture_geometry_after = Read-DesktopGeometry $after
        if (($after.client_bounds -join ',') -ne ($ExpectedBounds -join ',') -or
            ($after.bounds -join ',') -ne ($before.bounds -join ',')) { throw 'Target client/window bounds changed during desktop capture' }
        $path = Join-Path $OutputDirectory 'workspace.png'
        $bitmap.Save($path, [Drawing.Imaging.ImageFormat]::Png)
        if ((Get-Item -LiteralPath $path).Length -le 0) { throw 'Desktop capture produced no PNG bytes' }
        return @{ source='desktop-target-client-area'; bounds=$ExpectedBounds; window_bounds=$before.bounds; foreground_before=$before.foreground
            foreground_after=$after.foreground; sha256=(Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() }
    } finally { $bitmap.Dispose() }
}

function Invoke-NativeGuiProbe {
    $report.initial_guard = Assert-Target
    $report.identity_verified = $true
    Prepare-TargetWindow
    Focus-Target
    $report.input_guard = Assert-Target $true
    $report.foreground_verified = $true
    Wait-TargetDocument
    $report.accessibility_tree_read = $true
    $report.focus_before = Read-TargetFocus
    if (-not $report.focus_before.belongs_to_target -or -not $report.focus_before.runtime_id) { throw 'Initial focus is not in the bound target tree' }
    $null = Assert-Target $true
    $report.sendinput_count = Send-TargetTab
    if ($report.sendinput_count -ne 2) { throw 'SendInput did not accept both Tab key events; UIPI or input blocking may apply' }
    $report.native_tab_sent = $true
    for ($attempt=0; $attempt -lt 10; $attempt++) {
        $null = Assert-Target $true
        $report.focus_after = Read-TargetFocus
        if (-not $report.focus_after.belongs_to_target) { throw 'Focus left the bound target tree' }
        if ($report.focus_after.runtime_id -and $report.focus_after.runtime_id -ne $report.focus_before.runtime_id -and
            $report.focus_after.in_document -and $report.focus_after.keyboard_focusable) { break }
        Pause-Probe
    }
    if (-not $report.focus_after.runtime_id -or $report.focus_after.runtime_id -eq $report.focus_before.runtime_id -or
        -not $report.focus_after.in_document -or -not $report.focus_after.keyboard_focusable) { throw 'No observed focus change within the original WebView document after Tab' }
    $report.focus_changed_in_target_document = $true
    $guard = Assert-Target $true
    $report.screenshot = Capture-Target $guard.client_bounds
    $report.target_desktop_screenshot_saved = $true
    $report.status = 'native-connectivity-passed'
}
