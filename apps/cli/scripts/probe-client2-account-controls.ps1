function Send-OfflineKey([int]$Key, [string]$ExpectedRuntimeId = '', [switch]$RecoveryDismiss) {
    if ($RecoveryDismiss) { $report.offline.recovery_dismiss_detail = 'before_guard' }
    $null = Assert-Target $true
    if ($RecoveryDismiss) { $report.offline.recovery_dismiss_detail = 'before_focus' }
    $focus = Read-TargetFocus
    if (-not $focus.belongs_to_target -or -not $focus.in_document -or $focus.offscreen) { throw 'Offline input focus escaped the original visible document' }
    if ($ExpectedRuntimeId -and $focus.runtime_id -ne $ExpectedRuntimeId) { throw 'Offline button focus changed before key dispatch' }
    if ($RecoveryDismiss) { $report.offline.recovery_dismiss_detail = 'escape_send' }
    if ([RespireNativeInput]::Key([IntPtr][long]$binding.hwnd, [uint16]$Key) -ne 2) { throw 'Offline SendInput key was rejected' }
    if ($RecoveryDismiss) { $report.offline.recovery_dismiss_detail = 'after_guard' }
    $null = Assert-Target $true
}
function New-AccountStabilityClock { return [Diagnostics.Stopwatch]::StartNew() }
function Pause-AccountStability { Start-Sleep -Milliseconds 100 }
function New-AccountStabilityPidDiagnostic($RawPid, $HeldMember, $Depth, $AncestorDocument, $Snapshot) {
    # Existing failed sample only: no provider reads, coercion, identity admission or retries.
    $kind='non_int'
    if ($null -eq $RawPid) { $kind='null' }
    elseif ($RawPid -is [int]) {
        if ($RawPid -le 0) { $kind='nonpositive' } else { $kind='positive_not_in_tree' }
    }
    $value=@{pid_kind=$kind;held_member='unknown';depth='unknown';ancestor_document='unknown';in_same_snapshot='unknown'}
    if ($HeldMember -is [bool]) { $value.held_member=$HeldMember }
    if ($Depth -is [int] -and $Depth -ge 0 -and $Depth -le 32) { $value.depth=$Depth }
    if ($AncestorDocument -is [bool]) { $value.ancestor_document=$AncestorDocument }
    # Inspect only the already captured local CIM objects, and only after rejection.
    # Absent from this snapshot does not establish a legitimate descendant race.
    if ($RawPid -is [int] -and $RawPid -gt 0 -and $Snapshot -is [array]) {
        try {
            $present=$false; $complete=$true
            foreach ($item in $Snapshot) {
                $snapshotPid=$item.ProcessId
                if (($snapshotPid -isnot [int] -and $snapshotPid -isnot [uint32]) -or $snapshotPid -lt 0) { $complete=$false; continue }
                if ($snapshotPid -eq $RawPid) { $present=$true }
            }
            if ($complete) { $value.in_same_snapshot=$present }
        } catch { $value.in_same_snapshot='unknown' }
    }
    return $value
}
function Read-AccountStabilityButton([string]$Name, $Identities, $WindowClock) {
    $null=Assert-Target $true
    $window=Get-OfflineReplacementRoot; $walker=Get-OfflineReplacementWalker
    $walk=[Diagnostics.Stopwatch]::StartNew()
    $queue=[Collections.Generic.Queue[object]]::new(); $queue.Enqueue(@{element=$window;depth=0;document=$false})
    $visited=0; $found=$null; $foundId=''; $buttonCount=0
    while ($queue.Count -gt 0) {
        if ($visited -ge 512 -or $walk.Elapsed.TotalSeconds -ge 5 -or $WindowClock.Elapsed.TotalSeconds -ge 15) { throw 'Candidate stability traversal exceeded its bound' }
        $next=$queue.Dequeue(); $visited++
        $current=$next.element.Current; $rawNodePid=$null; $nodePid=[int]($rawNodePid=$current.ProcessId)
        if (-not $ownedIds.Contains($nodePid)) {
            $report.offline.account_stability_pid=New-AccountStabilityPidDiagnostic $rawNodePid ($Identities.ContainsKey($nodePid)) $next.depth $next.document $script:candidateProcessSnapshot
            throw 'Candidate stability process left the bound tree'
        }
        Assert-DocumentNodeIdentity $nodePid $Identities
        $type=$current.ControlType
        if ($type -isnot [Windows.Automation.ControlType]) { throw 'Candidate stability control type unavailable' }
        $document=$next.document -or $type -eq [Windows.Automation.ControlType]::Document
        if ($document -and $type -eq [Windows.Automation.ControlType]::Button) {
            $nodeName=$current.Name
            if ($nodeName -isnot [string]) { throw 'Candidate stability button name unavailable' }
            if ($nodeName -match $Name) {
                $buttonCount++
                if ($buttonCount -gt 1) { throw 'Candidate stability selector is ambiguous' }
                $parts=@($next.element.GetRuntimeId())
                if ($parts.Count -eq 0 -or @($parts | Where-Object { $_ -isnot [int] }).Count -gt 0) { throw 'Candidate stability RuntimeId unavailable' }
                $enabled=$current.IsEnabled; $focusable=$current.IsKeyboardFocusable; $offscreen=$current.IsOffscreen
                if ($enabled -isnot [bool] -or -not $enabled -or
                    $focusable -isnot [bool] -or -not $focusable -or
                    $offscreen -isnot [bool] -or $offscreen) { throw 'Candidate stability button is not focus-ready' }
                $found=$next.element; $foundId=$parts -join '.'
            }
        }
        $child=$walker.GetFirstChild($next.element)
        if ($child -and $next.depth -ge 32) { throw 'Candidate stability depth bound reached' }
        while ($child) {
            if ($visited+$queue.Count -ge 512 -or $walk.Elapsed.TotalSeconds -ge 5 -or $WindowClock.Elapsed.TotalSeconds -ge 15) { throw 'Candidate stability traversal exceeded its bound' }
            $queue.Enqueue(@{element=$child;depth=$next.depth+1;document=$document})
            $child=$walker.GetNextSibling($child)
        }
    }
    if ($buttonCount -ne 1 -or $walk.Elapsed.TotalSeconds -ge 5 -or $WindowClock.Elapsed.TotalSeconds -ge 15) { throw 'Candidate stability unique button unavailable' }
    foreach ($id in $Identities.Keys) { Assert-DocumentNodeIdentity ([int]$id) $Identities }
    return @{element=$found;runtime_id=$foundId}
}
function Get-AccountStabilityFocusedElement { return [Windows.Automation.AutomationElement]::FocusedElement }
function Get-AccountStabilityRawWalker { return [Windows.Automation.TreeWalker]::RawViewWalker }
function Read-AccountStabilityFocus($Identities, $WindowClock) {
    if ($WindowClock.Elapsed.TotalSeconds -ge 15) { throw 'Candidate stability focus observation exceeded its bound' }
    $null=Assert-Target $true
    $window=Get-OfflineReplacementRoot
    $rootPid=[int]$window.Current.ProcessId
    if (-not $ownedIds.Contains($rootPid)) { throw 'Candidate stability root is foreign' }
    Assert-DocumentNodeIdentity $rootPid $Identities
    $rootParts=@($window.GetRuntimeId())
    if ($rootParts.Count -eq 0 -or @($rootParts | Where-Object { $_ -isnot [int] }).Count -gt 0) { throw 'Candidate stability root identity unavailable' }
    $rootId=$rootParts -join '.'
    $element=Get-AccountStabilityFocusedElement
    if ($null -eq $element) { throw 'Candidate stability has no focused element' }
    $walker=Get-AccountStabilityRawWalker
    $focusedId=''; $offscreen=$null; $inDocument=$false; $belongs=$false
    for ($depth=0; $null -ne $element -and $depth -lt 32; $depth++) {
        if ($WindowClock.Elapsed.TotalSeconds -ge 15) { throw 'Candidate stability focus observation exceeded its bound' }
        # Only PID is read before the held lifetime is verified. No Name, Value,
        # AutomationId or ClassName is read anywhere in this focus observer.
        $current=$element.Current; $nodePid=[int]$current.ProcessId
        if (-not $ownedIds.Contains($nodePid)) { throw 'Candidate stability focused ancestry is foreign' }
        Assert-DocumentNodeIdentity $nodePid $Identities
        $parts=@($element.GetRuntimeId())
        if ($parts.Count -eq 0 -or @($parts | Where-Object { $_ -isnot [int] }).Count -gt 0) { throw 'Candidate stability focused identity unavailable' }
        $id=$parts -join '.'
        if ($depth -eq 0) {
            $focusedId=$id; $offscreen=$current.IsOffscreen
            if ($offscreen -isnot [bool]) { throw 'Candidate stability focused visibility unavailable' }
        }
        $type=$current.ControlType
        if ($type -isnot [Windows.Automation.ControlType]) { throw 'Candidate stability focused control type unavailable' }
        if ($type -eq [Windows.Automation.ControlType]::Document) { $inDocument=$true }
        if ($id -ceq $rootId) { $belongs=$true; break }
        $element=$walker.GetParent($element)
    }
    if (-not $belongs -or -not $inDocument) { throw 'Candidate stability focus is outside the original document' }
    foreach ($id in $Identities.Keys) { Assert-DocumentNodeIdentity ([int]$id) $Identities }
    if ($WindowClock.Elapsed.TotalSeconds -ge 15) { throw 'Candidate stability focus observation exceeded its bound' }
    return @{runtime_id=$focusedId;belongs_to_target=$belongs;in_document=$inDocument;offscreen=$offscreen}
}
function Assert-AccountFocusStable($Element, [string]$Name) {
    $observation=@{status='observing';requested_seconds=10;completed_poll_count='not-observed';observations=0
        scope='source-candidate timed UI stability only';pending_error_cancel='not-evaluated'}
    $report.offline.account_focus_stability=$observation
    $parts=@($Element.GetRuntimeId())
    if ($parts.Count -eq 0 -or @($parts | Where-Object { $_ -isnot [int] }).Count -gt 0) { throw 'Candidate initial RuntimeId unavailable' }
    $expected=$parts -join '.'
    $clock=New-AccountStabilityClock
    $observedStart=$null
    $held=[Collections.Generic.List[object]]::new(); $identities=@{}
    try {
        $null=Assert-Target $true
        foreach ($id in $ownedIds) {
            if ($clock.Elapsed.TotalSeconds -ge 15) { throw 'Candidate stability identity acquisition exceeded its bound' }
            $process=Get-Process -Id $id -ErrorAction Stop; $held.Add($process)
            Assert-DocumentProcessHandle $process
            $identities[[int]$id]=@{process=$process;birth=$process.StartTime.ToUniversalTime().Ticks;session=$process.SessionId;image=$process.Path}
        }
        $null=Assert-Target $true
        foreach ($id in $identities.Keys) {
            if (-not $ownedIds.Contains([int]$id)) { throw 'Candidate stability initial ancestry changed' }
            Assert-DocumentNodeIdentity ([int]$id) $identities
        }
        do {
            $button=Read-AccountStabilityButton $Name $identities $clock
            if ($button.runtime_id -cne $expected) { throw 'Candidate account button was replaced during stability window' }
            $null=Assert-Target $true
            $focus=Read-AccountStabilityFocus $identities $clock
            if ($focus.belongs_to_target -isnot [bool] -or -not $focus.belongs_to_target -or
                $focus.in_document -isnot [bool] -or -not $focus.in_document -or
                $focus.offscreen -isnot [bool] -or $focus.offscreen -or $focus.runtime_id -cne $expected) { throw 'Candidate account focus changed during stability window' }
            foreach ($id in $identities.Keys) { Assert-DocumentNodeIdentity ([int]$id) $identities }
            if ($clock.Elapsed.TotalSeconds -ge 15) { throw 'Candidate stability observation exceeded its bound' }
            if ($null -eq $observedStart) { $observedStart=$clock.Elapsed.TotalSeconds }
            $observation.observations++
            if ($clock.Elapsed.TotalSeconds-$observedStart -ge 10) { break }
            Pause-AccountStability
        } while ($true)
        $observation.status='timed-window-passed'; $observation.elapsed_ms=$clock.Elapsed.TotalMilliseconds
        $observation.observed_window_ms=($clock.Elapsed.TotalSeconds-$observedStart)*1000
    } finally { foreach ($process in $held) { $process.Dispose() } }
}
function Get-OfflineButtonInvoke($Element, [ValidateSet('normal_stop')][string]$DiagnosticStage, $RuntimeParts = $null) {
    $report.offline.preinvoke_identity = $null
    $report.offline.final_preparation.kind='unknown'
    $report.offline.final_preparation.proof='not_eligible'
    if ($DiagnosticStage) { $report.offline.action_stage = 'normal_stop_pattern' }
    if (-not $PSBoundParameters.ContainsKey('RuntimeParts')) { $RuntimeParts = @($Element.GetRuntimeId()) }
    $pattern = $null
    if (-not $Element.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
        throw 'Offline button does not support the normal UIA Invoke action'
    }
    if ($DiagnosticStage) { $report.offline.action_stage = 'normal_stop_focus_guard' }
    $null = Assert-Target $true
    return @{element=$Element;parts=$RuntimeParts;pattern=$pattern}
}
function Prepare-OfflineButtonInvoke($Element, [ref]$MismatchEvidence, [ValidateSet('normal_stop')][string]$DiagnosticStage, $Invocation = $null) {
    $MismatchEvidence.Value = $null
    if ($null -eq $report.offline.final_preparation) {
        $report.offline.final_preparation = @{route='bounded_attempt';attempt=0;kind='not_reached';proof='not_eligible'}
    }
    $report.offline.final_preparation.kind='unknown'
    $report.offline.final_preparation.proof='not_eligible'
    $preparationClock = [Diagnostics.Stopwatch]::StartNew()
    if ($null -eq $Invocation) {
        # Source-candidate stability keeps its already established focus window.
        $diagnosticArgs = @{}
        if ($DiagnosticStage) { $diagnosticArgs.DiagnosticStage = $DiagnosticStage }
        $Invocation = Get-OfflineButtonInvoke $Element @diagnosticArgs
    }
    $parts = @($Invocation.parts)
    $expected = $parts -join '.'
    $pattern = $Invocation.pattern
    $focus = Read-TargetFocus
    # Read each observation exactly once: PS5 may suppress a failing property
    # getter to null. Missing, unknown or malformed observations fail closed.
    $belongs = $focus.belongs_to_target
    $inDocument = $focus.in_document
    $offscreen = $focus.offscreen
    $focusedId = $focus.runtime_id
    if ($belongs -isnot [bool] -or $inDocument -isnot [bool] -or $offscreen -isnot [bool] -or
        $focusedId -isnot [string] -or $focusedId -cnotmatch '\A-?[0-9]+(?:\.-?[0-9]+)*\z' -or
        $parts.Count -eq 0 -or @($parts | Where-Object { $_ -isnot [int] }).Count -gt 0) {
        if ($belongs -isnot [bool] -or $inDocument -isnot [bool] -or $offscreen -isnot [bool]) {
            $report.offline.final_preparation.kind='invalid_focus_fields'
        } elseif ($focusedId -isnot [string] -or $focusedId -cnotmatch '\A-?[0-9]+(?:\.-?[0-9]+)*\z') {
            $report.offline.final_preparation.kind='invalid_focused_id'
        } else { $report.offline.final_preparation.kind='invalid_requested_id' }
        throw 'Offline button focus changed before UIA Invoke'
    }
    # These are the already validated observations, not additional provider reads.
    $report.offline.preinvoke_identity = @{expected=$expected;actual=$focusedId
        preparation_ms=[Math]::Round($preparationClock.Elapsed.TotalMilliseconds)}
    if (-not $belongs -or -not $inDocument -or $offscreen -or $focusedId -cne $expected) {
        if (-not $belongs) { $report.offline.final_preparation.kind='outside_target' }
        elseif (-not $inDocument) { $report.offline.final_preparation.kind='outside_document' }
        elseif ($offscreen) { $report.offline.final_preparation.kind='offscreen' }
        else { $report.offline.final_preparation.kind='id_mismatch' }
        # A mismatch alone is not replacement evidence. Only permit a complete
        # fresh-tree proof when focus remains in the original visible document.
        if ($belongs -and $inDocument -and -not $offscreen) {
            $MismatchEvidence.Value = @{original_id=$expected}
        }
        throw 'Offline button focus changed before UIA Invoke'
    }
    $report.offline.final_preparation.kind='verified'
    return @{element=$Element;pattern=$pattern}
}
function Press-OfflineButton([string]$Name, [switch]$VerifySourceStability, [ValidateSet('normal_stop')][string]$DiagnosticStage) {
    $report.offline.final_preparation = $null
    $report.offline.final_preparation_history = @()
    $report.offline.preinvoke_identity = $null
    $diagnosticArgs = @{}
    if ($DiagnosticStage) { $diagnosticArgs.DiagnosticStage = $DiagnosticStage }
    if ($DiagnosticStage) { $report.offline.action_stage = 'normal_stop_select' }
    if ($VerifySourceStability) {
        $report.offline.final_preparation = @{route='candidate_window';attempt=0;kind='not_reached';proof='not_eligible'}
        $report.offline.final_preparation_history = @($report.offline.final_preparation)
        # Candidate stability remains a separate observation window. Never refocus
        # or retry after this window and never borrow its time for replacement.
        $element = Find-FocusedOfflineButton $Name
        Assert-AccountFocusStable $element $Name
        $mismatchEvidence = $null
        $prepared = Prepare-OfflineButtonInvoke $element ([ref]$mismatchEvidence) @diagnosticArgs
    } else {
        $prepared = Find-FocusedOfflineButton $Name -PrepareInvoke @diagnosticArgs
    }
    # Effects and their postcondition checks are outside every retry handler.
    if ($DiagnosticStage) { $report.offline.action_stage = 'normal_stop_invoke' }
    $prepared.pattern.Invoke()
    if ($DiagnosticStage) { $report.offline.action_stage = 'normal_stop_post_invoke_guard' }
    $null = Assert-Target $true
}
function New-OfflineFieldClock { return [Diagnostics.Stopwatch]::StartNew() }
function Assert-OfflineFieldIdentity($Element, [string]$ExpectedId, $Identities) {
    $null = Assert-Target $true
    foreach ($id in $Identities.Keys) {
        if (-not $ownedIds.Contains([int]$id)) { throw 'Original field process ancestry changed' }
        Assert-DocumentNodeIdentity ([int]$id) $Identities
    }
    $nodePid = $Element.GetCurrentPropertyValue([Windows.Automation.AutomationElement]::ProcessIdProperty, $true)
    if ($nodePid -isnot [int] -or -not $ownedIds.Contains($nodePid)) { throw 'Original field process is unproven' }
    Assert-DocumentNodeIdentity $nodePid $Identities
    $parts = @($Element.GetRuntimeId())
    if ($parts.Count -eq 0 -or @($parts | Where-Object { $_ -isnot [int] }).Count -gt 0) { throw 'Original field identity unavailable' }
    $id = $parts -join '.'
    if ($ExpectedId -and $id -cne $ExpectedId) { throw 'Original field identity changed' }
    $password = $Element.GetCurrentPropertyValue([Windows.Automation.AutomationElement]::IsPasswordProperty, $true)
    $offscreen = $Element.GetCurrentPropertyValue([Windows.Automation.AutomationElement]::IsOffscreenProperty, $true)
    $enabled = $Element.GetCurrentPropertyValue([Windows.Automation.AutomationElement]::IsEnabledProperty, $true)
    if ($password -isnot [bool] -or $password -or $offscreen -isnot [bool] -or $offscreen -or
        $enabled -isnot [bool] -or -not $enabled) { throw 'Original field is not a known visible non-password input' }
    return $id
}
function Read-OfflineFieldValue($Element, [string]$ExpectedId, $Pattern, $Identities, $Clock, [switch]$Postcondition) {
    if ($Postcondition -and $Clock.Elapsed.TotalSeconds -ge 3) { throw 'Synthetic text was not observed in the original visible control' }
    $null = Assert-OfflineFieldIdentity $Element $ExpectedId $Identities
    # This reviewed focus reader validates PID/lifetime before ancestry and never
    # reads arbitrary names, values, AutomationId or ClassName.
    $focus = Read-AccountStabilityFocus $Identities $Clock
    if ($focus.belongs_to_target -isnot [bool] -or -not $focus.belongs_to_target -or
        $focus.in_document -isnot [bool] -or -not $focus.in_document -or
        $focus.offscreen -isnot [bool] -or $focus.offscreen -or
        $focus.runtime_id -isnot [string] -or $focus.runtime_id -cne $ExpectedId) {
        throw 'Visible text focus changed before readback'
    }
    if ($Postcondition -and $Clock.Elapsed.TotalSeconds -ge 3) { throw 'Synthetic text was not observed in the original visible control' }
    $value = $Pattern.Current.Value
    foreach ($id in $Identities.Keys) { Assert-DocumentNodeIdentity ([int]$id) $Identities }
    if ($Postcondition -and $Clock.Elapsed.TotalSeconds -ge 3) { throw 'Synthetic text was not observed in the original visible control' }
    if ($value -isnot [string]) { throw 'Original field value is unavailable' }
    return $value
}
function Fill-OfflineField([string]$Name, [string]$Text) {
    $held = [Collections.Generic.List[object]]::new(); $identities = @{}
    $acquisitionClock = [Diagnostics.Stopwatch]::StartNew()
    try {
        for ($attempt=0; $attempt -lt 2; $attempt++) {
            $remaining = 10 - $acquisitionClock.Elapsed.TotalSeconds
            if ($remaining -le 0) { throw 'Original field identity acquisition exceeded its selector budget' }
            $element = Find-OfflineControl 'ControlType.Edit' ('^' + [regex]::Escape($Name) + '$') $remaining
            $null = Assert-Target $true
            $exitedCandidate = $false
            foreach ($id in $ownedIds) {
                try { $process = Get-Process -Id $id -ErrorAction Stop } catch {
                    # A vanished snapshot candidate permits one fresh selector before input.
                    # Identity/permission failures and the bound actors never permit resampling.
                    if ($_.FullyQualifiedErrorId -cne 'NoProcessFoundForGivenId,Microsoft.PowerShell.Commands.GetProcessCommand' -or
                        $_.TargetObject -isnot [int] -or $_.TargetObject -ne $id -or
                        $id -eq $binding.desktop_pid -or $id -eq $binding.browser_pid -or
                        $attempt -ne 0 -or $acquisitionClock.Elapsed.TotalSeconds -ge 10) { throw }
                    $browser = @($candidateProcessSnapshot | Where-Object { $_.ProcessId -eq $binding.browser_pid })
                    if ($browser.Count -ne 1) { throw }
                    $cursor = [int]$id
                    for ($depth=0; $depth -lt 32 -and $cursor -ne $binding.browser_pid; $depth++) {
                        $candidate = @($candidateProcessSnapshot | Where-Object { $_.ProcessId -eq $cursor })
                        if ($candidate.Count -ne 1 -or $candidate[0].SessionId -ne $binding.session_id -or
                            -not [string]::Equals($candidate[0].ExecutablePath,$browser[0].ExecutablePath,[StringComparison]::OrdinalIgnoreCase)) { throw }
                        $cursor = [int]$candidate[0].ParentProcessId
                    }
                    if ($cursor -ne $binding.browser_pid -or
                        @(Get-CimInstance Win32_Process -Filter "ProcessId=$id").Count -ne 0) { throw }
                    $exitedCandidate = $true
                    break
                }
                $held.Add($process)
                Assert-DocumentProcessHandle $process
                $identities[[int]$id] = @{process=$process;birth=$process.StartTime.ToUniversalTime().Ticks;session=$process.SessionId;image=$process.Path}
            }
            if (-not $exitedCandidate) { break }
            foreach ($process in $held) { $process.Dispose() }
            $held.Clear(); $identities.Clear()
        }
        if ($acquisitionClock.Elapsed.TotalSeconds -ge 10) { throw 'Original field identity acquisition exceeded its selector budget' }
        $originalId = Assert-OfflineFieldIdentity $element '' $identities
        $pattern = $element.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern)
        $initialValue = $pattern.Current.Value
        if ($initialValue -isnot [string] -or -not [string]::Equals($initialValue,'',[StringComparison]::Ordinal)) { throw 'Offline synthetic field was not empty' }
        Focus-OfflineControl $element
        $clock = New-OfflineFieldClock
        $beforeInput = Read-OfflineFieldValue $element $originalId $pattern $identities $clock
        if (-not [string]::Equals($beforeInput,'',[StringComparison]::Ordinal)) { throw 'Offline synthetic field was not empty' }
        if ([RespireNativeInput]::Text([IntPtr][long]$binding.hwnd, $Text) -ne 2*$Text.Length) { throw 'Offline Unicode SendInput was incomplete' }
        # New, explicit postcondition budget: at most three seconds after the one
        # input dispatch. No refocus, replacement lookup, or input replay occurs.
        $clock.Restart()
        while ($clock.Elapsed.TotalSeconds -lt 3) {
            $value = Read-OfflineFieldValue $element $originalId $pattern $identities $clock -Postcondition
            if ($clock.Elapsed.TotalSeconds -ge 3) { break }
            if ([string]::Equals($value,$Text,[StringComparison]::Ordinal)) { return }
            Pause-Probe
        }
        throw 'Synthetic text was not observed in the original visible control'
    } finally { foreach ($process in $held) { $process.Dispose() } }
}

function Read-CoreQueryControl([string]$Mode = '', [switch]$FailureEvidence) {
    $options = @{Uri=($binding.core_query.address + '/__fixture'); Headers=@{'X-Fixture-Control'=$script:coreQueryControl.nonce}; TimeoutSec=10}
    if ($Mode) { $options.Method='POST'; $options.ContentType='application/json'; $options.Body=(@{mode=$Mode} | ConvertTo-Json -Compress) }
    $value = Invoke-RestMethod @options
    $publicKeys = @('operation_id','apply_ids','result_ids','result_issued','result_forwarded','result_response_lost','dropped_apply_replies','errors','core_test_mode','scope','apply_fusion','effective_fusion')
    $actualKeys = @($value.PSObject.Properties.Name)
    if ($actualKeys.Count -ne $publicKeys.Count -or @($actualKeys | Where-Object { $_ -cnotin $publicKeys }).Count) { throw 'Forwarding proof has non-public fields' }
    if ($value.core_test_mode -ne $false -or (-not $FailureEvidence -and @($value.errors).Count -ne 0)) { throw 'Real forwarding fixture failed' }
    return $value
}
function Read-CoreQueryRecord {
    $records = @(Get-Content -Raw -LiteralPath (Join-Path $binding.data_directory 'client2/requests.json') | ConvertFrom-Json)
    $jobs = @($records | Where-Object { $_.name -ceq 'agent_set' -and $_.transaction.kind -ceq 'core_config' })
    if ($jobs.Count -ne 1 -or $jobs[0].transaction.request_bound -ne $true -or
        $jobs[0].id -cne ('desktop-' + $jobs[0].transaction.operation_id)) { throw 'Expected one durable RequestBound core operation' }
    return $jobs[0]
}
function Find-CoreQuerySave {
    $combo = Find-OfflineControl 'ControlType.ComboBox' ([regex]::Unescape('\u005e\u878d\u5408\u6a21\u5f0f\u0024')) -Seconds 30
    $walker = [Windows.Automation.TreeWalker]::ControlViewWalker
    $parent = $walker.GetParent($combo)
    while ($parent -and $parent.Current.ControlType -ne [Windows.Automation.ControlType]::Document) {
        $null = Assert-Target $true
        $condition = [Windows.Automation.AndCondition]::new(
            [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::Button),
            [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,([regex]::Unescape('\u4fdd\u5b58'))))
        $buttons = $parent.FindAll([Windows.Automation.TreeScope]::Descendants,$condition)
        if ($buttons.Count -gt 1) { throw 'Fusion save control is ambiguous' }
        if ($buttons.Count -eq 1) { return $buttons[0] }
        $parent = $walker.GetParent($parent)
    }
    throw 'Fusion form save control is not identifiable'
}
function Invoke-CoreQueryAcceptance {
    $report.offline = @{}
    $report.core_query = @{status='failed';phase=$binding.core_query.phase;diagnostic_only=$true;scope_rejection_boundary='CLI';call_site='core_control_binding'}
    $script:coreQueryControl = Get-Content -Raw -LiteralPath $binding.core_query.control_file | ConvertFrom-Json
    if ($script:coreQueryControl.address -cne $binding.core_query.address -or $script:coreQueryControl.nonce -cnotmatch '^[a-f0-9]{64}$') { throw 'Private control binding differs' }
    if ($env:RSRS_CORE_TEST_MODE) { throw 'Real query mode cannot run with CORE_TEST_MODE' }
    $null = Read-CoreQueryControl
    $report.core_query.call_site = 'navigate_models'
    Press-OfflineButton ([regex]::Unescape('\u005e\u6a21\u578b\u4e0e\u7d22\u5f15\u0024'))
    $report.core_query.call_site = 'find_fusion_save'
    $save = Find-CoreQuerySave
    if ($binding.core_query.phase -ceq 'core-issue') {
        if (-not $save.Current.IsEnabled) { throw 'Fresh fusion save is disabled' }
        $report.core_query.call_site = 'find_fusion_combo'
        $combo = Find-OfflineControl 'ControlType.ComboBox' ([regex]::Unescape('\u005e\u878d\u5408\u6a21\u5f0f\u0024'))
        $report.core_query.call_site = 'fusion_combo_patterns'
        $report.core_query.control_type = $combo.Current.ControlType.ProgrammaticName
        $report.core_query.supported_patterns = @($combo.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName })
        $report.core_query.call_site = 'fusion_combo_focus'
        Focus-OfflineControl $combo
        $comboId = $combo.GetRuntimeId() -join '.'
        # The sealed HTML select orders legacy, attention. Runtime receipt proves the selection.
        $report.core_query.call_site = 'fusion_combo_home'
        Send-OfflineKey 0x24 $comboId
        $report.core_query.call_site = 'fusion_combo_down'
        Send-OfflineKey 0x28 $comboId
        $report.core_query.call_site = 'fusion_combo_tab'
        Send-OfflineKey 0x09 $comboId
        $report.core_query.selection_screenshot = Capture-Target (Assert-Target $true).client_bounds
        Copy-Item -LiteralPath (Join-Path $OutputDirectory 'workspace.png') -Destination (Join-Path $OutputDirectory 'core-selection.png')
        $report.core_query.call_site = 'fusion_arm_reply_loss'
        $null = Read-CoreQueryControl 'lose_reply'
        $mismatch = $null
        $report.core_query.call_site = 'fusion_save_focus'
        $save = Find-CoreQuerySave
        Focus-OfflineControl $save ([ref]$mismatch)
        $report.core_query.call_site = 'fusion_save_prepare'
        $prepared = Prepare-OfflineButtonInvoke $save ([ref]$mismatch)
        $report.core_query.call_site = 'fusion_save_invoke'
        $prepared.pattern.Invoke()
    } elseif ($binding.core_query.phase -cne 'core-resume') { throw 'Unknown core query phase' }
    $report.core_query.call_site = 'await_unknown'
    $querySelector = [regex]::Unescape('\u005e\u67e5\u8be2\u539f\u8bf7\u6c42\u0024')
    $null = Find-OfflineControl 'ControlType.Button' $querySelector -Seconds 60
    $null = Find-OfflineControl 'ControlType.Text' ([regex]::Unescape('\u005e\u6838\u5fc3\u914d\u7f6e\u7ed3\u679c\u672a\u786e\u8ba4\uff1b\u8bf7\u67e5\u8be2\u539f\u8bf7\u6c42\uff0c\u4e0d\u8981\u91cd\u65b0\u4fdd\u5b58\u3002\u0024')) -Seconds 30
    if ((Find-CoreQuerySave).Current.IsEnabled) { throw 'Unknown permits another save' }
    $job = Read-CoreQueryRecord
    $state = Read-CoreQueryControl
    if ($job.state -cne 'unconfirmed' -or @($state.apply_ids).Count -ne 1 -or
        $state.apply_ids[0] -cne $job.transaction.operation_id -or $state.dropped_apply_replies -ne 1 -or
        $state.apply_fusion -cne 'attention' -or $state.effective_fusion -cne 'attention' -or
        ($job.transaction.scope | ConvertTo-Json -Compress) -cne ($state.scope | ConvertTo-Json -Compress)) { throw 'Unknown is not the single real applied operation' }
    $report.core_query.transaction = $job.transaction
    $report.core_query.unknown_screenshot = Capture-Target (Assert-Target $true).client_bounds
    Copy-Item -LiteralPath (Join-Path $OutputDirectory 'workspace.png') -Destination (Join-Path $OutputDirectory ($binding.core_query.phase + '-unknown.png'))
    if ($binding.core_query.phase -ceq 'core-resume') {
        $before = @($state.result_ids).Count
        $beforeForwarded = $state.result_forwarded
        $beforeLost = $state.result_response_lost
        $report.core_query.call_site = 'query_original_lost_reply'
        Press-OfflineButton $querySelector
        $null = Find-OfflineControl 'ControlType.Text' ([regex]::Unescape('\u005e\u6838\u5fc3\u914d\u7f6e\u7ed3\u679c\u4ecd\u672a\u786e\u8ba4\uff1b\u4fdd\u7559\u539f\u8bf7\u6c42\uff0c\u4e0d\u4f1a\u91cd\u65b0\u5e94\u7528\u3002\u0024')) -Seconds 60
        $failed = Read-CoreQueryRecord
        $state = Read-CoreQueryControl
        if ($failed.state -cne 'unconfirmed' -or @($state.result_ids).Count -le $before -or
            $state.result_forwarded -le $beforeForwarded -or $state.result_response_lost -le $beforeLost -or
            $state.result_forwarded -ne $state.result_issued -or (Find-CoreQuerySave).Current.IsEnabled) { throw 'Failed button query erased Unknown or did not lose a real Runtime result response' }
        $null = Read-CoreQueryControl 'pass'
        $report.core_query.call_site = 'query_original_applied'
        Press-OfflineButton $querySelector
        $null = Find-OfflineControl 'ControlType.Text' ([regex]::Unescape('\u005e\u4e94\u8f74\u878d\u5408\u914d\u7f6e\u5df2\u4fdd\u5b58\uff0c\u6838\u5fc3\u5df2\u5e94\u7528\uff1b\u4e0b\u4e00\u6b21\u53ec\u56de\u4f7f\u7528\u5df2\u786e\u8ba4\u7684\u8fd0\u884c\u914d\u7f6e\u3002\u0024')) -Seconds 60
        $done = Read-CoreQueryRecord
        if ($done.state -cne 'done' -or -not (Find-CoreQuerySave).Current.IsEnabled -or
            ($done.transaction | ConvertTo-Json -Compress) -cne ($job.transaction | ConvertTo-Json -Compress)) { throw 'Original query did not close the same RequestBound operation' }
        $state = Read-CoreQueryControl
        if (@($state.apply_ids).Count -ne 1 -or @($state.result_ids | Where-Object { $_ -cne $job.transaction.operation_id }).Count) { throw 'Query replayed apply or used another operation' }
        # The UI never accepts caller scope. Check foreign scope at the real CLI boundary.
        $foreign = @{scope=@{account_id=$job.transaction.scope.account_id;instance_id=$job.transaction.scope.instance_id;generation=($job.transaction.scope.generation + 1)};
            operation_id=$job.transaction.operation_id;idempotency_key=$job.transaction.operation_id}
        $start = [Diagnostics.ProcessStartInfo]::new($binding.cli_exe)
        $start.UseShellExecute=$false; $start.CreateNoWindow=$true; $start.RedirectStandardOutput=$true; $start.RedirectStandardError=$true
        $start.Arguments = '--json runtime core-config-result --context "' + (($foreign | ConvertTo-Json -Depth 5 -Compress).Replace('"','\"')) + '" --source-profile "' + $job.transaction.source_profile.TrimEnd('\') + '"'
        if ((Get-FileHash -LiteralPath $binding.cli_exe -Algorithm SHA256).Hash.ToLowerInvariant() -cne $binding.cli_sha256) { throw 'Scope query CLI bytes changed' }
        $journal = Join-Path $binding.data_directory 'client2/requests.json'
        $marker = Join-Path $binding.data_directory ('runtime/autostart/core-config-results/' + $job.transaction.operation_id + '.json')
        $journalHash = (Get-FileHash -LiteralPath $journal -Algorithm SHA256).Hash
        $markerHash = (Get-FileHash -LiteralPath $marker -Algorithm SHA256).Hash
        $originalContext = @{scope=$job.transaction.scope;operation_id=$job.transaction.operation_id;idempotency_key=$job.transaction.operation_id}
        $foreignArguments = $start.Arguments
        $start.Arguments = '--json runtime core-config-result --context "' + (($originalContext | ConvertTo-Json -Depth 5 -Compress).Replace('"','\"')) + '" --source-profile "' + $job.transaction.source_profile.TrimEnd('\') + '"'
        $valid = [Diagnostics.Process]::Start($start)
        try {
            $validOut = $valid.StandardOutput.ReadToEndAsync(); $validErr = $valid.StandardError.ReadToEndAsync()
            if (-not $valid.WaitForExit(30000)) { throw 'Original terminal query timed out' }
            $originalReply = $validOut.GetAwaiter().GetResult() | ConvertFrom-Json
            $validErr.GetAwaiter().GetResult() | Out-Null
            if ($valid.ExitCode -ne 0 -or $originalReply.summary.core_application.state -cne 'applied' -or
                $originalReply.details.operation_id -cne $job.transaction.operation_id) { throw 'Original CLI context cannot query its real terminal result' }
        } finally { if (-not $valid.HasExited) { $valid.Kill(); $valid.WaitForExit() }; $valid.Dispose() }
        $start.Arguments = $foreignArguments
        $query = [Diagnostics.Process]::Start($start)
        try {
            $output = $query.StandardOutput.ReadToEndAsync(); $errors = $query.StandardError.ReadToEndAsync()
            if (-not $query.WaitForExit(30000)) { throw 'Foreign scope query timed out' }
            $value = $output.GetAwaiter().GetResult() | ConvertFrom-Json
            $errors.GetAwaiter().GetResult() | Out-Null
            if ($query.ExitCode -eq 0 -or $value.status -cne 'fail' -or $value.summary.core_application.state -ceq 'applied') { throw 'Foreign scope was accepted' }
        } finally { if (-not $query.HasExited) { $query.Kill(); $query.WaitForExit() }; $query.Dispose() }
        if ((Get-FileHash -LiteralPath $journal -Algorithm SHA256).Hash -cne $journalHash -or
            (Get-FileHash -LiteralPath $marker -Algorithm SHA256).Hash -cne $markerHash) { throw 'Foreign scope query changed the original journal' }
        $report.core_query.foreign_scope_rejected = $true
        $state = Read-CoreQueryControl
        if (@($state.apply_ids).Count -ne 1 -or @($state.result_ids | Where-Object { $_ -cne $job.transaction.operation_id }).Count) { throw 'CLI boundary check replayed apply or queried another operation' }
        $report.core_query.applied_screenshot = Capture-Target (Assert-Target $true).client_bounds
        Copy-Item -LiteralPath (Join-Path $OutputDirectory 'workspace.png') -Destination (Join-Path $OutputDirectory 'core-applied.png')
        $report.core_query.final_state = 'applied'
    }
    $report.core_query.counts = $state
    $report.core_query.status = $binding.core_query.phase + '-passed'
}
