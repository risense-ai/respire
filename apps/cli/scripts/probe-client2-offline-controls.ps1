
# Inserted into the existing system-PS5 helper. These functions do not execute when parsed.
function Find-OfflineControl([string]$Type, [string]$Name, [double]$Seconds = 10, [switch]$AllowAbsent, [switch]$ObserveStartStopSelector) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    if ($ObserveStartStopSelector) {
        # Historical selector observations only, never an acceptance condition.
        $report.offline.runtime_binding_stage = 'stop_control'
        $report.offline.runtime_start_stop_selector = @{state='unobservable';complete_samples=0}
    }
    do {
        if ($ObserveStartStopSelector) {
            $report.offline.runtime_start_stop_selector.state='unobservable'
            $selectorMatches=0; $lastSelectorEnabled=$null; $selectorFieldsUnknown=$false
        }
        $null = Assert-Target $true
        $window = Get-OfflineReplacementRoot
        $walker = Get-OfflineReplacementWalker
        $queue = [Collections.Generic.Queue[object]]::new()
        $queue.Enqueue(@{element=$window; depth=0; document=$false})
        $found = [Collections.Generic.List[object]]::new(); $visited = 0
        $walk = [Diagnostics.Stopwatch]::StartNew()
        while ($queue.Count -and $visited -lt 512 -and $walk.Elapsed.TotalSeconds -lt 5) {
            $next = $queue.Dequeue(); $visited++; $current = $next.element.Current
            # Cache each original getter independently at its original short-circuit point.
            # Never merge the repeated ControlType reads or add diagnostic UIA reads.
            $unread=[object]::new()
            $selectorPid=$unread
            if (-not $ownedIds.Contains([int]($selectorPid=$current.ProcessId))) {
                $foreignType=$unread; $foreignTypeName=$unread; $foreignName=$unread
                $report.offline.foreign_node_matches_selector = (($foreignTypeName=($foreignType=$current.ControlType).ProgrammaticName) -eq $Type -and ($foreignName=$current.Name) -match $Name)
                if ($ObserveStartStopSelector) {
                    if ($null -eq $current -or $selectorPid -isnot [int] -or $selectorPid -le 0 -or
                        $foreignType -isnot [Windows.Automation.ControlType] -or $foreignTypeName -isnot [string] -or
                        (-not [object]::ReferenceEquals($foreignName,$unread) -and $foreignName -isnot [string])) { $selectorFieldsUnknown=$true }
                }
                if ($report.offline.foreign_node_matches_selector) { throw 'Offline UIA node escaped the bound process tree' }
                continue
            }
            $documentType=$unread
            $document = $next.document -or ($documentType=$current.ControlType) -eq [Windows.Automation.ControlType]::Document
            $matchType=$unread; $matchTypeName=$unread; $matchName=$unread; $selectorEnabled=$unread
            # Only a selector match is retained. Never serialize arbitrary UIA names or values:
            # the normal key-generation dialog contains recovery material.
            if ($document -and ($matchTypeName=($matchType=$current.ControlType).ProgrammaticName) -eq $Type -and
                ($matchName=$current.Name) -match $Name) {
                if ($ObserveStartStopSelector) { $selectorMatches++ }
                # Cache the original short-circuit getter; retain its original truthiness.
                if ($AllowAbsent -or ($selectorEnabled=$current.IsEnabled)) { $found.Add($next.element) }
                if ($ObserveStartStopSelector) { $lastSelectorEnabled=$selectorEnabled }
            }
            if ($ObserveStartStopSelector) {
                if ($null -eq $current -or $selectorPid -isnot [int] -or $selectorPid -le 0 -or
                    (-not [object]::ReferenceEquals($documentType,$unread) -and $documentType -isnot [Windows.Automation.ControlType]) -or
                    (-not [object]::ReferenceEquals($matchType,$unread) -and $matchType -isnot [Windows.Automation.ControlType]) -or
                    (-not [object]::ReferenceEquals($matchTypeName,$unread) -and $matchTypeName -isnot [string]) -or
                    (-not [object]::ReferenceEquals($matchName,$unread) -and $matchName -isnot [string]) -or
                    (-not [object]::ReferenceEquals($selectorEnabled,$unread) -and $selectorEnabled -isnot [bool])) { $selectorFieldsUnknown=$true }
            }
            if ($next.depth -lt 32) {
                $child = $walker.GetFirstChild($next.element)
                while ($child -and $visited + $queue.Count -lt 512 -and $walk.Elapsed.TotalSeconds -lt 5) {
                    $queue.Enqueue(@{element=$child; depth=$next.depth+1; document=$document})
                    $child = $walker.GetNextSibling($child)
                }
                if ($child) { throw 'Offline selector traversal truncated before uniqueness could be established' }
            } elseif ($walker.GetFirstChild($next.element)) {
                throw 'Offline selector depth limit prevents uniqueness verification'
            }
        }
        if ($queue.Count) { throw 'Offline selector traversal exceeded its node/time bound' }
        if ($ObserveStartStopSelector) {
            $diagnostic=$report.offline.runtime_start_stop_selector
            # Saturates: counts complete samples, not application poll completions.
            if ($diagnostic.complete_samples -lt 256) { $diagnostic.complete_samples++ }
            if ($selectorFieldsUnknown) { $diagnostic.state='unobservable' }
            elseif ($selectorMatches -eq 0) { $diagnostic.state='absent' }
            elseif ($selectorMatches -gt 1) { $diagnostic.state='ambiguous' }
            elseif ($lastSelectorEnabled -is [bool]) {
                if ($lastSelectorEnabled) { $diagnostic.state='enabled' } else { $diagnostic.state='disabled' }
            }
        }
        if ($found.Count -gt 1) { throw 'Offline selector is ambiguous' }
        if ($found.Count -eq 1) { return $found[0] }
        if ($AllowAbsent) { return $null }
        Pause-Probe
    } while ($clock.Elapsed.TotalSeconds -lt $Seconds)
    throw "Expected offline control unavailable: $Type / $Name"
}

function Focus-OfflineControl($Element, [ref]$MismatchEvidence) {
    if ($null -ne $MismatchEvidence) { $MismatchEvidence.Value = $null }
    $diagnostic = @{phase='target_before'; kind='pending'}
    $report.offline.focus_diagnostic = $diagnostic
    try {
        $null = Assert-Target $true
        $diagnostic.phase = 'expected_id'
        $expected = $Element.GetRuntimeId() -join '.'
        $diagnostic.phase = 'set_focus'
        $Element.SetFocus()
        $focusClock = [Diagnostics.Stopwatch]::StartNew()
        $diagnostic.phase = 'target_after'
        $null = Assert-Target $true
        $diagnostic.phase = 'read_focus'
        $focus = Read-TargetFocus
        $diagnostic.phase = 'match_focus'
        $diagnostic.after_focus_ms = [Math]::Round($focusClock.Elapsed.TotalMilliseconds)
        if ($expected -cmatch '\A-?[0-9]+(?:\.-?[0-9]+)*\z' -and
            $focus.runtime_id -is [string] -and $focus.runtime_id -cmatch '\A-?[0-9]+(?:\.-?[0-9]+)*\z') {
            $diagnostic.runtime_ids = @{expected=$expected;actual=$focus.runtime_id}
        }
        $diagnostic.observed = @{belongs_to_target=[bool]$focus.belongs_to_target
            in_document=[bool]$focus.in_document; expected_id_match=($focus.runtime_id -eq $expected)
            offscreen=[bool]$focus.offscreen}
        if (-not $focus.belongs_to_target -or -not $focus.in_document -or $focus.runtime_id -ne $expected -or $focus.offscreen) {
            $diagnostic.kind = 'focus_mismatch'
            if ($null -ne $MismatchEvidence -and $expected -and
                $focus.belongs_to_target -is [bool] -and $focus.belongs_to_target -and
                $focus.in_document -is [bool] -and $focus.in_document -and
                $focus.offscreen -is [bool] -and -not $focus.offscreen -and $focus.runtime_id -ne $expected) {
                # Private evidence only; never include RuntimeIds in the report.
                $MismatchEvidence.Value = @{original_id=$expected}
            }
            $detail = @{requested_id_available=$false; requested_id_matches_original=$false
                requested_focus_available=$false; requested_has_keyboard_focus=$false
                focused_type_available=$false; focused_is_button=$false}
            $diagnostic.mismatch_detail = $detail
            # Independent best-effort observations never replace the primary failure
            # or turn a mismatch into a retryable stale/disabled exception.
            try {
                $requestedId = @($Element.GetRuntimeId())
                if ($requestedId.Count -gt 0 -and @($requestedId | Where-Object { $_ -isnot [int] }).Count -eq 0) {
                    $detail.requested_id_available = $true
                    $detail.requested_id_matches_original = (($requestedId -join '.') -ceq $expected)
                }
            } catch {}
            try {
                $requestedFocus = $Element.Current.HasKeyboardFocus
                if ($requestedFocus -is [bool]) {
                    $detail.requested_focus_available = $true
                    $detail.requested_has_keyboard_focus = $requestedFocus
                }
            } catch {}
            try {
                $focusedType = $focus.control_type
                if ($focusedType -is [string] -and $focusedType.Length -gt 0) {
                    $detail.focused_type_available = $true
                    $detail.focused_is_button = ($focusedType -ceq 'ControlType.Button')
                }
            } catch {}
            throw 'Offline control did not receive real target-document keyboard focus'
        }
        $diagnostic.kind = 'verified'
    } catch {
        # Closed categories only. Preserve the original exception for the existing
        # stale/disabled retry policy, never serialize its message or UI properties.
        if ($diagnostic.kind -ne 'focus_mismatch') {
            $diagnostic.kind = 'other_exception'
            $current = $_.Exception
            for ($depth=0; $current -and $depth -lt 4; $depth++) {
                if ($current -is [Windows.Automation.ElementNotAvailableException]) { $diagnostic.kind='stale'; break }
                if ($current -is [Windows.Automation.ElementNotEnabledException]) { $diagnostic.kind='disabled'; break }
                $current = $current.InnerException
            }
        }
        throw
    }
}
# Only known UIA stale/disabled failures permit reacquisition; arbitrary provider
# exceptions remain failures. Never serialize exception messages or control values.
function Test-OfflineFocusRetryable($Exception) {
    $current = $Exception
    for ($depth=0; $current -and $depth -lt 4; $depth++) {
        if ($current -is [Windows.Automation.ElementNotAvailableException] -or
            $current -is [Windows.Automation.ElementNotEnabledException]) { return $true }
        $current = $current.InnerException
    }
    return $false
}
function Get-OfflineReplacementRoot {
    return [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$binding.hwnd)
}
function Get-OfflineReplacementWalker { return [Windows.Automation.TreeWalker]::ControlViewWalker }
function Test-OfflineButtonReplacement([string]$Name, [string]$OriginalId, $AttemptClock, [double]$Seconds = 10) {
    if (-not $OriginalId -or $AttemptClock.Elapsed.TotalSeconds -ge $Seconds) { return $false }
    $null = Assert-Target $true
    if ($AttemptClock.Elapsed.TotalSeconds -ge $Seconds) { return $false }
    $held = [Collections.Generic.List[object]]::new()
    $identities = @{}
    $walk = [Diagnostics.Stopwatch]::StartNew()
    try {
    $ids = [Collections.Generic.HashSet[int]]::new()
    foreach ($knownPid in $ownedIds) {
        if ($walk.Elapsed.TotalSeconds -ge 5 -or $AttemptClock.Elapsed.TotalSeconds -ge $Seconds) { return $false }
        $process = Get-Process -Id $knownPid -ErrorAction Stop
        $held.Add($process)
        Assert-DocumentProcessHandle $process
        $identities[[int]$knownPid] = @{process=$process; birth=$process.StartTime.ToUniversalTime().Ticks
            session=$process.SessionId; image=$process.Path}
        [void]$ids.Add([int]$knownPid)
    }
    # Ancestry is resampled while these exact process lifetimes remain held.
    $null = Assert-Target $true
    foreach ($knownPid in $ids) {
        if (-not $ownedIds.Contains($knownPid)) { return $false }
        Assert-DocumentNodeIdentity $knownPid $identities
    }
    if ($walk.Elapsed.TotalSeconds -ge 5 -or $AttemptClock.Elapsed.TotalSeconds -ge $Seconds) { return $false }
    $window = Get-OfflineReplacementRoot
    $walker = Get-OfflineReplacementWalker
    $queue = [Collections.Generic.Queue[object]]::new()
    $queue.Enqueue(@{element=$window; depth=0; document=$false})
    $visited = 0; $candidate = $null; $candidateId = ''; $replacementMatchCount = 0
    while ($queue.Count -gt 0) {
        if ($visited -ge 512 -or $walk.Elapsed.TotalSeconds -ge 5 -or $AttemptClock.Elapsed.TotalSeconds -ge $Seconds) { return $false }
        $next = $queue.Dequeue(); $visited++
        $current = $next.element.Current
        # Foreign processes fail before any Name or other diagnostic value is read.
        if (-not $ids.Contains([int]$current.ProcessId)) { return $false }
        Assert-DocumentNodeIdentity ([int]$current.ProcessId) $identities
        $runtimeParts = @($next.element.GetRuntimeId())
        if ($runtimeParts.Count -eq 0 -or @($runtimeParts | Where-Object { $_ -isnot [int] }).Count -gt 0) { return $false }
        $runtimeId = $runtimeParts -join '.'
        if ($runtimeId -ceq $OriginalId) { return $false }
        $controlType = $current.ControlType
        if ($controlType -isnot [Windows.Automation.ControlType]) { return $false }
        $document = $next.document -or $controlType -eq [Windows.Automation.ControlType]::Document
        $matchesName = $false
        if ($document -and $controlType -eq [Windows.Automation.ControlType]::Button) {
            $nodeName = $current.Name
            if ($nodeName -isnot [string]) { return $false }
            $matchesName = $nodeName -match $Name
        }
        # Disabled/hidden matches still count: they must not hide ambiguity.
        if ($matchesName) {
            $replacementMatchCount++
            if ($replacementMatchCount -gt 1) { return $false }
            $candidate = $next.element; $candidateId = $runtimeId
        }
        $child = $walker.GetFirstChild($next.element)
        if ($child -and $next.depth -ge 32) { return $false }
        while ($child) {
            if ($visited + $queue.Count -ge 512 -or $walk.Elapsed.TotalSeconds -ge 5 -or $AttemptClock.Elapsed.TotalSeconds -ge $Seconds) { return $false }
            $queue.Enqueue(@{element=$child; depth=$next.depth+1; document=$document})
            $child = $walker.GetNextSibling($child)
        }
    }
    if ($replacementMatchCount -ne 1 -or $candidateId -ceq $OriginalId -or
        $walk.Elapsed.TotalSeconds -ge 5 -or $AttemptClock.Elapsed.TotalSeconds -ge $Seconds) { return $false }
    $null = Assert-Target $true
    foreach ($knownPid in $ids) {
        if (-not $ownedIds.Contains($knownPid)) { return $false }
        Assert-DocumentNodeIdentity $knownPid $identities
    }
    $current = $candidate.Current
    if (-not $ids.Contains([int]$current.ProcessId) -or -not $ownedIds.Contains([int]$current.ProcessId)) { return $false }
    Assert-DocumentNodeIdentity ([int]$current.ProcessId) $identities
    $controlType = $current.ControlType
    if ($controlType -isnot [Windows.Automation.ControlType] -or $controlType -ne [Windows.Automation.ControlType]::Button) { return $false }
    $nodeName = $current.Name
    if ($nodeName -isnot [string]) { return $false }
    $enabled=$current.IsEnabled; $focusable=$current.IsKeyboardFocusable; $offscreen=$current.IsOffscreen
    if (($candidate.GetRuntimeId() -join '.') -cne $candidateId -or
        $nodeName -notmatch $Name -or
        $enabled -isnot [bool] -or -not $enabled -or
        $focusable -isnot [bool] -or -not $focusable -or
        $offscreen -isnot [bool] -or $offscreen) { return $false }
    foreach ($knownPid in $ids) { Assert-DocumentNodeIdentity $knownPid $identities }
    return ($walk.Elapsed.TotalSeconds -lt 5 -and $AttemptClock.Elapsed.TotalSeconds -lt $Seconds)
    } finally { foreach ($process in $held) { $process.Dispose() } }
}
function Find-FocusedOfflineButton([string]$Name, [switch]$PrepareInvoke, [ValidateSet('normal_stop')][string]$DiagnosticStage) {
    if ($PrepareInvoke) { $report.offline.final_preparation_history = @() }
    $diagnosticArgs = @{}
    if ($DiagnosticStage) { $diagnosticArgs.DiagnosticStage = $DiagnosticStage }
    $scrollAttempts = 0
    for ($attempt=0; $attempt -lt 5; $attempt++) {
        $attemptClock = [Diagnostics.Stopwatch]::StartNew()
        $mismatchEvidence = $null
        $preparingInvoke = $false
        $preparationTimedOut = $false
        if ($PrepareInvoke) {
            $report.offline.preinvoke_identity = $null
            $report.offline.final_preparation = @{route='bounded_attempt';attempt=($attempt+1);kind='not_reached';proof='not_eligible'}
            $report.offline.final_preparation_history = @($report.offline.final_preparation_history) + @($report.offline.final_preparation)
        }
        if ($DiagnosticStage) { $report.offline.action_stage = 'normal_stop_select' }
        $report.offline.focus_diagnostic = $null
        $null = Assert-Target $true
        # Selector traversal still proves ownership and uniqueness. Selector errors,
        # including ambiguity, are deliberately outside the retry handler.
        $remaining = 10 - $attemptClock.Elapsed.TotalSeconds
        if ($remaining -le 0) { throw 'Offline button focus failed without a retryable UIA readiness condition' }
        $element = Find-OfflineControl 'ControlType.Button' $Name $remaining
        try {
            $state = $element.Current
            $report.offline.focus_readiness = @{ enabled=[bool]$state.IsEnabled
                focusable=[bool]$state.IsKeyboardFocusable; offscreen=[bool]$state.IsOffscreen }
            if ($state.IsEnabled -and $state.IsKeyboardFocusable -and $state.IsOffscreen) {
                if ($scrollAttempts -ge 2) { $report.offline.focus_scroll = 'ineffective'; throw 'Offline button remained offscreen after bounded normal scrolling' }
                $scroll = $null
                if (-not $element.TryGetCurrentPattern([Windows.Automation.ScrollItemPattern]::Pattern, [ref]$scroll)) {
                    $report.offline.focus_scroll = 'unsupported'
                    throw 'Offline offscreen button does not support normal UIA scrolling'
                }
                $null = Assert-Target $true
                $scrollAttempts++
                $report.offline.focus_scroll = 'requested'
                $report.offline.focus_scroll_attempts = $scrollAttempts
                $scroll.ScrollIntoView()
                $null = Assert-Target $true
                # Never focus or invoke the pre-scroll element: reacquire it through
                # the complete ownership/document/uniqueness traversal next time.
                Pause-Probe
                continue
            }
            if (-not $state.IsEnabled -or -not $state.IsKeyboardFocusable -or $state.IsOffscreen) {
                Pause-Probe
                continue
            }
            if ($scrollAttempts -gt 0) { $report.offline.focus_scroll = 'visible' }
            $invocation = $null
            if ($PrepareInvoke) {
                # A stale initial identity uses the existing focus retry path;
                # pattern acquisition and final preparation remain fail-closed.
                if ($DiagnosticStage) { $report.offline.action_stage = 'normal_stop_pattern' }
                $runtimeParts = @($element.GetRuntimeId())
                $preparingInvoke = $true
                if ($attemptClock.Elapsed.TotalSeconds -ge 10) { $preparationTimedOut=$true; throw 'Offline button preparation exceeded its existing attempt budget' }
                # Acquire the pattern and its full guard before establishing focus:
                # overview refreshes replace buttons while these UIA/CIM reads run.
                $invocation = Get-OfflineButtonInvoke $element @diagnosticArgs -RuntimeParts $runtimeParts
                if ($attemptClock.Elapsed.TotalSeconds -ge 10) { $preparationTimedOut=$true; throw 'Offline button preparation exceeded its existing attempt budget' }
                $preparingInvoke = $false
            }
            Focus-OfflineControl $element ([ref]$mismatchEvidence)
            if ($PrepareInvoke) {
                # Continue this same attempt, never start a second retry budget.
                $preparingInvoke = $true
                $mismatchEvidence = $null
                if ($attemptClock.Elapsed.TotalSeconds -ge 10) { $preparationTimedOut=$true; throw 'Offline button preparation exceeded its existing attempt budget' }
                $prepared = Prepare-OfflineButtonInvoke $element ([ref]$mismatchEvidence) -Invocation $invocation @diagnosticArgs
                if ($attemptClock.Elapsed.TotalSeconds -ge 10) { $preparationTimedOut=$true; throw 'Offline button preparation exceeded its existing attempt budget' }
                return $prepared
            }
            return $element
        } catch {
            $primaryException = $_.Exception
            # Mirror the original short-circuit budget checks exactly once.
            $canProve = $false
            if ($null -ne $mismatchEvidence) {
                if ($attempt -lt 4) {
                    if ($attemptClock.Elapsed.TotalSeconds -lt 10) { $canProve=$true }
                    elseif ($preparingInvoke) { $report.offline.final_preparation.proof='budget_exhausted' }
                } elseif ($preparingInvoke) { $report.offline.final_preparation.proof='attempts_exhausted' }
            }
            if ($preparingInvoke -and $preparationTimedOut) { $report.offline.final_preparation.proof='budget_exhausted' }
            if ($canProve) {
                $replacement = $false
                if ($preparingInvoke) { $report.offline.final_preparation.proof='false' }
                try { $replacement = Test-OfflineButtonReplacement $Name $mismatchEvidence.original_id $attemptClock 10 } catch {
                    if ($preparingInvoke) { $report.offline.final_preparation.proof='exception' }
                }
                if ($replacement -is [bool] -and $replacement) {
                    if ($preparingInvoke) { $report.offline.final_preparation.proof='verified' }
                    # No effect was dispatched. Consume this existing attempt and
                    # reacquire through the ordinary full selector on the next one.
                    continue
                }
            }
            # Preparation is not a generic stale/disabled retry region. Only the
            # complete replacement proof above can consume another attempt.
            if ($preparingInvoke) { throw $primaryException }
            if (-not (Test-OfflineFocusRetryable $primaryException)) {
                throw 'Offline button focus failed without a retryable UIA readiness condition'
            }
        }
        Pause-Probe
    }
    throw 'Offline button did not become focus-ready within the bounded attempts'
}
