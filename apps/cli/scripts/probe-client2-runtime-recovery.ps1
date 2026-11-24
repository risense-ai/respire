function Read-OfflineHttp([string]$Path, $Body = $null) {
    if ($Path -notin @('/api/health','/api/rpc')) { throw 'Offline oracle permits only fixed local health/RPC paths' }
    $request = [Net.HttpWebRequest]::Create('http://127.0.0.1:' + $binding.rpc_port + $Path)
    $request.Proxy = $null; $request.Timeout = 10000; $request.ReadWriteTimeout = 10000
    # Match Client2's health request: verify the Runtime actor behind the CLI compatibility host.
    if ($Path -eq '/api/health') { $request.Headers['X-Respire-Health'] = 'runtime-v1' }
    if ($null -ne $Body) {
        $request.Method = 'POST'; $request.ContentType = 'application/json'
        $bytes = [Text.Encoding]::UTF8.GetBytes(($Body | ConvertTo-Json -Compress -Depth 6))
        $request.ContentLength = $bytes.Length
        $stream = $request.GetRequestStream()
        try { $stream.Write($bytes,0,$bytes.Length) } finally { $stream.Dispose() }
    }
    $response = $request.GetResponse()
    try {
        $stream = $response.GetResponseStream(); $output = [IO.MemoryStream]::new()
        try {
            $buffer = [byte[]]::new(8192)
            while (($count = $stream.Read($buffer,0,$buffer.Length)) -gt 0) {
                if ($output.Length + $count -gt 8*1024*1024) { throw 'Offline oracle response exceeded 8 MiB' }
                $output.Write($buffer,0,$count)
            }
            return ([Text.Encoding]::UTF8.GetString($output.ToArray()) | ConvertFrom-Json)
        } finally { $stream.Dispose(); $output.Dispose() }
    } finally { $response.Dispose() }
}
function Bind-OfflineRuntime {
    $report.offline.runtime_binding_stage = 'health'
    $health = Read-OfflineHttp '/api/health'
    $report.offline.runtime_binding_stage = 'process'
    $process = Get-Process -Id $health.pid -ErrorAction Stop
    $report.offline.runtime_binding_stage = 'cim'
    $record = Get-CimInstance Win32_Process -Filter "ProcessId=$($health.pid)"
    $report.offline.runtime_binding_stage = 'identity'
    # Rust canonicalize uses the extended Windows namespace for the same local image.
    $healthImage = [string]$health.exe
    if ($healthImage.StartsWith('\\?\', [StringComparison]::Ordinal)) { $healthImage = $healthImage.Substring(4) }
    # Record only original short-circuit comparisons; never export raw identity fields.
    $report.offline.runtime_binding_checks=@{}
    $identityChecks=$report.offline.runtime_binding_checks
    if (-not ($identityChecks.server=(-not ($health.server -ne 'respire'))) -or
        -not ($identityChecks.protocol=(-not ($health.v -ne 1))) -or
        -not ($identityChecks.health_image=[string]::Equals($healthImage,$binding.runtime_exe,[StringComparison]::OrdinalIgnoreCase)) -or
        -not ($identityChecks.process_image=[string]::Equals($process.Path,$binding.runtime_exe,[StringComparison]::OrdinalIgnoreCase)) -or
        -not ($identityChecks.data_directory=[string]::Equals([IO.Path]::GetFullPath($health.data_dir),$binding.data_directory,[StringComparison]::OrdinalIgnoreCase)) -or
        -not ($identityChecks.process_record=[bool]$record) -or
        -not ($identityChecks.session=(-not ($process.SessionId -ne $binding.session_id))) -or
        -not ($identityChecks.image_digest=(-not ((Get-FileHash -LiteralPath $process.Path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $binding.runtime_sha256)))) {
        throw 'Offline Runtime health/process/profile/hash binding failed'
    }
    $script:offlineRuntime = @{pid=$process.Id; started_ticks=$process.StartTime.ToUniversalTime().Ticks; record=$record}
    # Store process identity only, never the session/keygen payload.
    $report.offline.runtime_process = $record | Select-Object ProcessId,ParentProcessId,ExecutablePath,CreationDate,SessionId,@{Name="creation_utc_ticks";Expression={$_.CreationDate.ToUniversalTime().Ticks.ToString([Globalization.CultureInfo]::InvariantCulture)}}
    $report.offline.runtime_bound = $true
    $report.offline.runtime_binding_stage = 'bound'
    # Persist only the guarded actor record before credential verification or business action.
    # The parent can still clean this detached actor if this helper reaches its watchdog.
    [IO.File]::WriteAllText((Join-Path $OutputDirectory 'native-gui.json'),
        ($report | ConvertTo-Json -Depth 12) + "`n", [Text.UTF8Encoding]::new($false))
}
function Assert-OfflineRuntime {
    $health = Read-OfflineHttp '/api/health'
    $process = Get-Process -Id $offlineRuntime.pid -ErrorAction Stop
    if ($health.pid -ne $offlineRuntime.pid -or $process.StartTime.ToUniversalTime().Ticks -ne $offlineRuntime.started_ticks -or
        -not [string]::Equals($process.Path,$binding.runtime_exe,[StringComparison]::OrdinalIgnoreCase) -or
        -not [string]::Equals([IO.Path]::GetFullPath($health.data_dir),$binding.data_directory,[StringComparison]::OrdinalIgnoreCase)) {
        throw 'Offline Runtime actor changed during business acceptance'
    }
    return $health
}
function Read-OfflineRpc([string[]]$Arguments) {
    # Read-only oracle: no CLI/API writes may stand in for visible UI operations.
    if (($Arguments -join '|') -notmatch '^--json\|(list\|--limit\|100|show\|[A-Za-z0-9_-]+|diary|status|recall\|gui-offline-[0-9a-f]{32} important body\|--mode\|fast\|--titles\|--limit\|20)$') { throw 'Offline oracle command is not allowlisted' }
    $health = Assert-OfflineRuntime
    # Same identity contract as native_fixture_process.scoped_rpc_request/verify_rpc_receipt.
    if ($health.v -ne 1 -or $health.account_id -isnot [string] -or -not $health.account_id -or
        $health.instance_id -isnot [string] -or -not $health.instance_id -or
        ($health.generation -isnot [int] -and $health.generation -isnot [long] -and $health.generation -isnot [uint64]) -or
        $health.generation -le 0) { throw 'RPC requires complete health scope' }
    $scope = @{account_id=$health.account_id; instance_id=$health.instance_id; generation=$health.generation}
    $id = 'offline-gui-' + [Guid]::NewGuid().ToString('N')
    $operation = 'offline-gui-' + [Guid]::NewGuid().ToString('N')
    $reply = Read-OfflineHttp '/api/rpc' @{v=1; id=$id; operation_id=$operation; idempotency_key=$operation; scope=$scope; method='cli.exec'; args=$Arguments}
    if ($reply.operation_id -cne $operation -or $reply.idempotency_key -cne $operation -or
        $reply.scope.account_id -cne $scope.account_id -or $reply.scope.instance_id -cne $scope.instance_id -or
        $reply.scope.generation -ne $scope.generation -or $reply.exit -ne 0) { throw 'Read-only oracle receipt scope/operation mismatch' }
    if ($reply.v -ne 1 -or $reply.id -ne $id -or $reply.ok -ne $true -or
        ($reply.envelope.status -ne 'ok' -and -not ($Arguments[1] -ceq 'recall' -and $reply.envelope.status -ceq 'skip')) -or
        $reply.envelope.command -ne $Arguments[1] -or
        @($reply.envelope.errors).Count -ne 0) { throw 'Read-only oracle did not return a matching successful receipt' }
    $null = Assert-OfflineRuntime
    return $reply.envelope
}
function Wait-OfflinePreferences([string]$Field, $Expected, $Observation = $null) {
    for ($attempt=0; $attempt -lt 50; $attempt++) {
        $preferences = Get-Content -Raw -LiteralPath $binding.preferences_path | ConvertFrom-Json
        if ($null -ne $Observation) { $Observation.actual_pref = $preferences.$Field }
        if ($preferences.$Field -eq $Expected) { return }
        Pause-Probe
    }
    $report.offline.preference_observation = @{
        onboarding_completed=($preferences.onboardingCompleted -eq $true)
        step=$(if ($preferences.onboardingStep -in @('language','sync','model','done')) { $preferences.onboardingStep } else { 'other' })
        sync_choice=$(if ($preferences.onboardingSyncChoice -in @('offline','cloud','')) { $preferences.onboardingSyncChoice } else { 'other' })
    }
    throw "Original preferences did not persist the visible action: $Field"
}
function Set-OfflineAutoRuntime([bool]$Enabled) {
    $phase = if ($Enabled) { 'enable' } else { 'disable' }
    $report.offline.action_stage = 'settings_' + $phase
    $preference = Find-OfflineControl 'ControlType.CheckBox' ([regex]::Unescape('\u005e\u81ea\u52a8\u8fde\u63a5\u0020\u0052\u0075\u006e\u0074\u0069\u006d\u0065\u0024'))
    $parts = @($preference.GetRuntimeId())
    if ($parts.Count -eq 0 -or @($parts | Where-Object { $_ -isnot [int] }).Count -gt 0) { throw 'AutoRuntime checkbox identity unavailable' }
    $expectedId = $parts -join '.'
    $toggle = $preference.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern)
    $before = $toggle.Current.ToggleState
    if ($before -isnot [Windows.Automation.ToggleState] -or $before -eq [Windows.Automation.ToggleState]::Indeterminate) { throw 'AutoRuntime checkbox state unknown' }
    if ($Enabled -and $before -ne [Windows.Automation.ToggleState]::Off) { throw 'AutoRuntime was not initially disabled' }
    $observation = @{before=$before.ToString(); after='not_observed'; expected_pref=$Enabled; actual_pref=$null}
    $report.offline.settings_inputs[$phase] = $observation
    Focus-OfflineControl $preference
    Send-OfflineKey 32 -ExpectedRuntimeId $expectedId
    $null = Assert-Target $true
    if (($preference.GetRuntimeId() -join '.') -cne $expectedId) { throw 'AutoRuntime checkbox identity changed after Space' }
    $after = $toggle.Current.ToggleState
    if ($after -isnot [Windows.Automation.ToggleState] -or $after -eq [Windows.Automation.ToggleState]::Indeterminate) { throw 'AutoRuntime checkbox state unknown after Space' }
    $observation.after = $after.ToString()
    $report.offline.action_stage = 'settings_' + $phase + '_persist'
    Wait-OfflinePreferences 'autoRuntime' $Enabled $observation
}
function Create-OfflineMemory([string]$Title, [string]$Content, [bool]$Diary) {
    $report.offline.action_stage = $(if ($Diary) { 'diary_editor_open' } else { 'important_editor_open' })
    Press-OfflineButton ([regex]::Unescape('\u005e\u8bb0\u5fc6\u0028\u003f\u003a\u005c\u0073\u007c\u0024\u0029'))
    Press-OfflineButton ([regex]::Unescape('\u005e\u65b0\u5efa\u8bb0\u5fc6\u0024'))
    Fill-OfflineField ([regex]::Unescape('\u6807\u9898')) $Title
    Fill-OfflineField ([regex]::Unescape('\u6b63\u6587')) $Content
    if ($Diary) {
        Focus-OfflineControl (Find-OfflineControl 'ControlType.ComboBox' ([regex]::Unescape('\u005e\u5b58\u50a8\u533a\u57df\u0024')))
        Send-OfflineKey 35 # End selects the actual final option: \u65e5\u8bb0 \xb7 \u7410\u4e8b.
        Send-OfflineKey 9
    }
    Focus-OfflineControl (Find-OfflineControl 'ControlType.CheckBox' ([regex]::Unescape('\u005e\u660e\u786e\u65b0\u5efa\uff0c\u8df3\u8fc7\u5224\u91cd')))
    Send-OfflineKey 32
    $report.offline.action_stage = $(if ($Diary) { 'diary_save' } else { 'important_save' })
    Press-OfflineButton ([regex]::Unescape('\u005e\u4fdd\u5b58\u0024'))
    # Wait for the editor to close and the actual reader to render the supplied content.
    $null = Find-OfflineControl 'ControlType.Button' ([regex]::Unescape('\u005e\u7f16\u8f91\u0024')) 60
    $report.offline.action_stage = $(if ($Diary) { 'diary_created_reader' } else { 'important_created_reader' })
    $body = Find-OfflineControl 'ControlType.Text' ([regex]::Escape($Content))
    if ($body.Current.IsOffscreen) { throw 'Synthetic memory body is present in UIA but not visible' }
}
function Get-OfflineDiaryNavigationPattern {
    # Default Latin-digit locale counts: zero or comma-grouped positive integers.
    # Other separators/digit scripts remain unproven and fail closed.
    return ('\A' + [regex]::Unescape('\u65e5\u8bb0') + '(?: (?:0|[1-9][0-9]{0,2}(?:,[0-9]{3})*))?\z')
}
function Assert-OfflineDiaryReader([string]$Title, [string]$Content) {
    # Only the exact synthetic diary dialog is readable, never arbitrary Edit values.
    Wait-OfflineRecoveryDismissed
    $element = Find-OfflineControl 'ControlType.Edit' ('^' + [regex]::Escape($Title) + '$')
    if ($element.Current.IsOffscreen) { throw 'Synthetic diary reader is not visible' }
    $null = Assert-Target $true
    $pattern = $null
    if (-not $element.TryGetCurrentPattern([Windows.Automation.ValuePattern]::Pattern, [ref]$pattern)) { throw 'Synthetic diary reader has no value pattern' }
    if (-not $pattern.Current.IsReadOnly) { throw 'Synthetic diary reader is not read-only' }
    $value = [string]$pattern.Current.Value
    if (-not $value.Contains($Content)) { throw 'Synthetic diary reader does not contain the saved body' }
    $null = Assert-Target $true
}
function Close-OfflineDiaryReader([string]$Title) {
    $selector = '^' + [regex]::Escape($Title) + '$'
    $null = Find-OfflineControl 'ControlType.Edit' $selector
    Press-OfflineButton ([regex]::Unescape('\u005e\u5173\u95ed\u0024'))
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        $null = Assert-Target $true
        if (-not (Find-OfflineControl 'ControlType.Edit' $selector 1 -AllowAbsent)) { return }
        Pause-Probe
    } while ($clock.Elapsed.TotalSeconds -lt 10)
    throw 'Synthetic diary dialog did not close'
}
function Wait-OfflineRecoveryDismissed {
    $report.offline.recovery_dismiss_detail = 'observe_absence'
    $selector = '^' + [regex]::Escape([regex]::Unescape('\u8bf7\u4fdd\u5b58\u672c\u673a\u6062\u590d\u6750\u6599')) + '$'
    # Only fixed-name existence is inspected. Never read Edit.Value or arbitrary
    # tree text; a disabled recovery field still means the dialog is not dismissed.
    for ($attempt=0; $attempt -lt 10; $attempt++) {
        $edit = Find-OfflineControl 'ControlType.Edit' $selector 1 -AllowAbsent
        $title = Find-OfflineControl 'ControlType.Text' $selector 1 -AllowAbsent
        if ($null -eq $edit -and $null -eq $title) {
            $report.offline.recovery_dismiss_detail = 'complete'
            $report.offline.recovery_controls_absent = $true
            return
        }
        Pause-Probe
    }
    throw 'Original recovery dialog controls did not disappear within the bounded checks'
}
function Assert-OfflineLocalKeys($Envelope) {
    $value = $Envelope.summary.session.has_local_keys
    if ($value -isnot [bool]) { throw 'Offline status local-key flag is missing or not boolean' }
    $report.offline.has_local_keys = $value
    if (-not $value) { throw 'Offline status does not confirm local keys' }
}
function Convert-OfflineEntry($Value) {
    # The fixed frontend memory/model.js::entry accepts these exact envelope/entry shapes.
    if ($Value.details.entry) { $Value = $Value.details.entry }
    elseif ($Value.entry) { $Value = $Value.entry }
    if (-not ($Value.id -is [string]) -or -not $Value.id) { throw 'Runtime returned no valid memory entry' }
    return $Value
}
function Stop-OfflineRuntime {
    $report.offline.action_stage = 'runtime_pre_stop_check'
    $null = Assert-OfflineRuntime
    $report.offline.action_stage = 'overview_navigation'
    Press-OfflineButton ([regex]::Unescape('\u005e\u6982\u89c8\u0024'))
    Press-OfflineButton ([regex]::Unescape('\u005e\u505c\u6b62\u0020\u0052\u0075\u006e\u0074\u0069\u006d\u0065\u0024')) -DiagnosticStage 'normal_stop'
    $report.offline.action_stage = 'normal_stop_process_lookup'
    $process = Get-Process -Id $offlineRuntime.pid -ErrorAction SilentlyContinue
    if ($process) {
        $report.offline.action_stage = 'normal_stop_birth_check'
        if ($process.StartTime.ToUniversalTime().Ticks -ne $offlineRuntime.started_ticks) { $report.offline.failure_kind = 'pid_reused'; throw 'Runtime PID reused before normal-stop observation' }
        $report.offline.action_stage = 'normal_stop_wait_exit'
        if (-not $process.WaitForExit(30000)) { $report.offline.failure_kind = 'exit_timeout'; throw 'Runtime did not exit after visible normal Stop' }
    }
    for ($attempt=0; $attempt -lt 2; $attempt++) {
        if ($attempt -eq 0) { $report.offline.action_stage = 'normal_stop_cim_first' } else { $report.offline.action_stage = 'normal_stop_cim_second' }
        if (Get-CimInstance Win32_Process -Filter "ProcessId=$($offlineRuntime.pid)") { $report.offline.failure_kind = 'pid_present_after_stop'; throw 'Runtime PID still exists or was reused after normal Stop' }
        Pause-Probe
    }
    $report.offline.action_stage = 'normal_stop_confirmed'
    $report.offline.runtime_normal_stop_confirmed = $true
}
function Assert-OfflineRecall([string]$Id, [string]$Title, [string]$Query) {
    $report.offline.recall_hit = $false
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        $result = Read-OfflineRpc @('--json','recall',$Query,'--mode','fast','--titles','--limit','20')
        $details = ConvertTo-Json -InputObject $result.details -Compress -Depth 16
        if ($details.Contains(('"' + $Id + '"')) -and $details.Contains(('"' + $Title + '"'))) {
            $report.offline.recall_hit = $true
            return
        }
        # Only retry a successful read while the real index explicitly reports pending work.
        if ($result.summary.index_pending -le 0) { throw 'Recall did not return the exact GUI-written memory' }
        Pause-Probe
    } while ($clock.Elapsed.TotalSeconds -lt 60)
    throw 'GUI-written memory did not become recallable while indexing'
}
function Invoke-ExistingKeysAcceptance {
    $report.offline = [ordered]@{status='failed'; runtime_bound=$false; runtime_normal_stop_confirmed=$false
        automatic_start=$false; automatic_key_handoff=$false; full_gui_acceptance='not-evaluated'; g1_passed=$false}
    if ((-not $binding.final_public_assets -and -not $binding.verified_candidate_assets) -or -not $binding.restart_memory) { throw 'Existing-key restart requires final public assets and the original synthetic memory' }
    Wait-OfflinePreferences 'autoRuntime' $true
    Press-OfflineButton ([regex]::Unescape('^\u6982\u89c8$'))
    # Observe the automatic start; never press Start or call a CLI start/keygen command.
    $null = Find-OfflineControl 'ControlType.Button' ([regex]::Unescape('^\u505c\u6b62 Runtime$')) 60
    Bind-OfflineRuntime
    if ($offlineRuntime.started_ticks -lt $binding.desktop_started_ticks -or
        ($offlineRuntime.pid -eq $binding.previous_runtime_pid -and
         $offlineRuntime.started_ticks.ToString() -ceq $binding.previous_runtime_ticks)) { throw 'Restart reused an old Runtime actor' }
    $report.offline.automatic_start = $true
    Assert-OfflineLocalKeys (Read-OfflineRpc @('--json','status'))
    $saved = $binding.restart_memory
    $entry = Convert-OfflineEntry (Read-OfflineRpc @('--json','show',$saved.id))
    if ($entry.id -cne $saved.id -or $entry.title -cne $saved.title -or $entry.content -cne $saved.query) { throw 'Automatic key handoff did not reopen the original memory' }
    $report.offline.automatic_key_handoff = $true
    Assert-OfflineRecall $saved.id $saved.title $saved.query
    Stop-OfflineRuntime
    $report.offline.status = 'offline-subset-passed'
}
function Read-RecoveryRecord {
    # requests.json is the public desktop journal, never a CLI material/custody record.
    # PS5 emits a JSON array as one object; assign it directly so the next pipeline enumerates records.
    $records = Get-Content -Raw -LiteralPath (Join-Path $binding.data_directory 'client2/requests.json') | ConvertFrom-Json
    $matches = @($records | Where-Object { $_.name -ceq 'auth_reset' })
    if ($matches.Count -ne 1) { throw 'Require exactly one real desktop reset request' }
    $record = $matches[0]
    $allowed = @('id','name','started','state','host','transaction','materialSaved')
    if (@($record.PSObject.Properties.Name | Where-Object { $_ -cnotin $allowed }).Count -or
        @($record.transaction.PSObject.Properties.Name | Where-Object { $_ -cnotin @('operation_id','source_profile','kind') }).Count -or
        $record.id -cnotmatch '\Adesktop-[0-9a-f-]{36}\z' -or $record.state -cne 'done' -or
        $record.host -isnot [bool] -or $record.host -ne $true -or $record.transaction.kind -cne 'key' -or
        $record.transaction.source_profile -isnot [string] -or -not $record.transaction.source_profile -or
        ($null -ne $record.materialSaved -and $record.materialSaved -isnot [bool]) -or
        $record.transaction.operation_id -cnotmatch '\A[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\z') {
        # Export only fixed predicates, never journal values or unexpected field names.
        $report.recovery.journal_validation = [ordered]@{
            record_fields_public = @($record.PSObject.Properties.Name | Where-Object { $_ -cnotin $allowed }).Count -eq 0
            transaction_fields_public = @($record.transaction.PSObject.Properties.Name | Where-Object { $_ -cnotin @('operation_id','source_profile','kind') }).Count -eq 0
            request_id_valid = $record.id -cmatch '\Adesktop-[0-9a-f-]{36}\z'
            state_done = $record.state -ceq 'done'
            host_true = $record.host -is [bool] -and $record.host -eq $true
            transaction_kind_key = $record.transaction.kind -ceq 'key'
            source_profile_valid = $record.transaction.source_profile -is [string] -and [bool]$record.transaction.source_profile
            material_saved_type_valid = $null -eq $record.materialSaved -or $record.materialSaved -is [bool]
            operation_id_valid = $record.transaction.operation_id -cmatch '\A[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\z'
        }
        throw 'Reset journal must contain only a completed public transaction identity and materialSaved'
    }
    return $record
}
function Assert-RecoveryMaterialGate {
    $null = Find-OfflineControl 'ControlType.Text' ([regex]::Unescape('^\u8bf7\u4fdd\u5b58\u6062\u590d\u6750\u6599$')) 60
    $checkbox = Find-OfflineControl 'ControlType.CheckBox' ([regex]::Unescape('^\u6211\u5df2\u5c06\u6062\u590d\u6750\u6599\u4fdd\u5b58\u5230\u5b89\u5168\u4f4d\u7f6e$'))
    $toggle = $checkbox.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern)
    $submit = Find-OfflineControl 'ControlType.Button' ([regex]::Unescape('^\u6211\u5df2\u4fdd\u5b58\u6062\u590d\u6750\u6599$')) 10 -AllowAbsent
    if ($toggle.Current.ToggleState -ne [Windows.Automation.ToggleState]::Off -or
        $null -eq $submit -or $submit.Current.IsEnabled) { throw 'Unchecked recovery acknowledgement is not blocked' }
    Focus-OfflineControl $checkbox
    Send-OfflineKey 13 -ExpectedRuntimeId ($checkbox.GetRuntimeId() -join '.')
    $null = Find-OfflineControl 'ControlType.Text' ([regex]::Unescape('^\u8bf7\u4fdd\u5b58\u6062\u590d\u6750\u6599$'))
    if ($submit.Current.IsEnabled -or $toggle.Current.ToggleState -ne [Windows.Automation.ToggleState]::Off) {
        throw 'Enter bypassed the unchecked recovery acknowledgement'
    }
}
function Confirm-RecoveryMaterial {
    $checkbox = Find-OfflineControl 'ControlType.CheckBox' ([regex]::Unescape('^\u6211\u5df2\u5c06\u6062\u590d\u6750\u6599\u4fdd\u5b58\u5230\u5b89\u5168\u4f4d\u7f6e$'))
    $id = $checkbox.GetRuntimeId() -join '.'
    Focus-OfflineControl $checkbox
    Send-OfflineKey 32 -ExpectedRuntimeId $id
    if (($checkbox.GetRuntimeId() -join '.') -cne $id -or
        $checkbox.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern).Current.ToggleState -ne [Windows.Automation.ToggleState]::On) {
        throw 'Recovery checkbox did not accept actual Space input'
    }
    Press-OfflineButton ([regex]::Unescape('^\u6211\u5df2\u4fdd\u5b58\u6062\u590d\u6750\u6599$'))
    for ($attempt=0; $attempt -lt 50; $attempt++) {
        $title = Find-OfflineControl 'ControlType.Text' ([regex]::Unescape('^\u8bf7\u4fdd\u5b58\u6062\u590d\u6750\u6599$')) 1 -AllowAbsent
        $material = Find-OfflineControl 'ControlType.Edit' ([regex]::Unescape('^\u6062\u590d\u6750\u6599$')) 1 -AllowAbsent
        if ($null -eq $title -and $null -eq $material) {
            $script:recoveryMaterialVisible = $false
            return
        }
        Pause-Probe
    }
    throw 'Acknowledged recovery dialog did not disappear'
}
function Invoke-RecoveryMaterialAcceptance {
    $report.offline = @{} # Reuse guarded UI input diagnostics, not offline acceptance claims.
    $report.recovery = @{status='failed'; phase=$RecoveryPhase; unchecked_blocked=$false}
    if (-not $binding.final_public_assets -and -not $binding.verified_candidate_assets) { throw 'Recovery acceptance requires verified product bytes' }
    if ($RecoveryPhase -ceq 'verified') {
        $record = Read-RecoveryRecord
        if ($record.materialSaved -ne $true) { throw 'Saved recovery acknowledgement did not survive another restart' }
        $record.PSObject.Properties.Remove('materialSaved')
        if ($record.id -cne $binding.recovery_request.id -or
            $record.transaction.operation_id -cne $binding.recovery_request.transaction.operation_id -or
            $record.transaction.source_profile -cne $binding.recovery_request.transaction.source_profile) {
            throw 'Saved restart changed the original reset identity'
        }
        Press-OfflineButton ([regex]::Unescape('^\u8d26\u6237\u4e0e\u7a7a\u95f4$'))
        $null = Find-OfflineControl 'ControlType.Text' ([regex]::Unescape('^\u5df2\u4fdd\u5b58$'))
        for ($sample=0; $sample -lt 3; $sample++) {
            $title = Find-OfflineControl 'ControlType.Text' ([regex]::Unescape('^\u8bf7\u4fdd\u5b58\u6062\u590d\u6750\u6599$')) 1 -AllowAbsent
            $material = Find-OfflineControl 'ControlType.Edit' ([regex]::Unescape('^\u6062\u590d\u6750\u6599$')) 1 -AllowAbsent
            if ($null -ne $title -or $null -ne $material) { throw 'Saved recovery material was prompted again after restart' }
        }
        $script:recoveryMaterialVisible = $false
        $report.recovery.material_saved = $true
        $report.recovery.saved_restart_no_prompt = $true
        $report.recovery.absence_samples = 3
        Invoke-NativeGuiProbe
        $report.recovery.status = 'recovery-subset-passed'
        return
    }
    if ($RecoveryPhase -ceq 'issue') {
        Press-OfflineButton ([regex]::Unescape('^\u8d26\u6237\u4e0e\u7a7a\u95f4$'))
        $script:recoveryMaterialVisible = $true
        Press-OfflineButton ([regex]::Unescape('^\u66f4\u6362\u8d85\u7ea7\u5bc6\u7801$'))
        Press-OfflineButton ([regex]::Unescape('^\u66f4\u6362\u5e76\u663e\u793a\u65b0\u6750\u6599$'))
        Press-OfflineButton ([regex]::Unescape('^\u66f4\u6362$'))
    }
    Assert-RecoveryMaterialGate
    $record = Read-RecoveryRecord
    if ($record.materialSaved -eq $true) { throw 'Unacknowledged reset was persisted as saved' }
    $report.recovery.unchecked_blocked = $true
    $report.recovery.request = $record
    if ($RecoveryPhase -ceq 'resume') {
        $script:recoveryMaterialVisible = $true
        if ($record.id -cne $binding.recovery_request.id -or
            $record.transaction.operation_id -cne $binding.recovery_request.transaction.operation_id -or
            $record.transaction.source_profile -cne $binding.recovery_request.transaction.source_profile) { throw 'Restart did not recover the original reset identity' }
        Confirm-RecoveryMaterial
        $saved = Read-RecoveryRecord
        if ($saved.materialSaved -ne $true) { throw 'Real recovery acknowledgement did not persist materialSaved' }
        $saved.PSObject.Properties.Remove('materialSaved')
        $record.PSObject.Properties.Remove('materialSaved')
        if (($saved | ConvertTo-Json -Compress -Depth 6) -cne ($record | ConvertTo-Json -Compress -Depth 6)) {
            throw 'Recovery acknowledgement changed the original transaction identity'
        }
        $report.recovery.material_saved = $true
        # Run ordinary focus/capture acceptance only after all material dialogs disappear.
        Invoke-NativeGuiProbe
    }
    $report.recovery.status = 'recovery-subset-passed'
}
function Invoke-OfflineGuiAcceptance {
    $report.offline = [ordered]@{status='failed'; onboarding_completed=$false; runtime_bound=$false
        local_keys_initialized=$false; important_memory_readback=$false; diary_readback=$false
        settings_persisted=$false; runtime_normal_stop_confirmed=$false; failure_kind='unknown'
        full_gui_acceptance='not-evaluated'; g1_passed=$false}
    $report.offline.onboarding_stage = 'choose_offline'
    # Normal first-run route; do not use the separate \u201cskip wizard\u201d action or edit completion markers.
    Press-OfflineButton ([regex]::Unescape('\u005e\u4ec5\u5728\u6b64\u8bbe\u5907\u4f7f\u7528\u0028\u003f\u003a\u005c\u0073\u007c\u0024\u0029'))
    $report.offline.onboarding_stage = 'advance_sync'
    Press-OfflineButton ([regex]::Unescape('\u005e\u4e0b\u4e00\u6b65\u0024'))
    $report.offline.onboarding_stage = 'await_model_step'
    Wait-OfflinePreferences 'onboardingStep' 'model'
    Wait-OfflinePreferences 'onboardingSyncChoice' 'offline'
    $report.offline.onboarding_stage = 'open_model_confirmation'
    Press-OfflineButton ([regex]::Unescape('\u005e\u6682\u4e0d\u4e0b\u8f7d\u0024'))
    $confirmation = Find-OfflineControl 'ControlType.Text' ([regex]::Unescape('\u005e\u6682\u4e0d\u4e0b\u8f7d\u6a21\u578b\u0024'))
    if ($confirmation.Current.IsOffscreen) { throw 'Original model confirmation title is not visible' }
    $report.offline.onboarding_stage = 'confirm_model_skip'
    Press-OfflineButton ([regex]::Unescape('\u005e\u6682\u4e0d\u4e0b\u8f7d\u0024')) # Original confirmation dialog.
    Wait-OfflinePreferences 'onboardingCompleted' $true
    Wait-OfflinePreferences 'onboardingSyncChoice' 'offline'
    $report.offline.onboarding_completed = $true
    $report.offline.onboarding_stage = 'completed'
    # Host keygen intentionally completes without automatically starting a serving owner.
    # Initialize first, then use the normal explicit Start action and bind the first actor.
    $report.offline.action_stage = 'account_navigation'
    Press-OfflineButton ([regex]::Unescape('\u005e\u8d26\u6237\u4e0e\u7a7a\u95f4\u0024'))
    $report.offline.action_stage = 'offline_initialization'
    Press-OfflineButton ([regex]::Unescape('\u005e\u521d\u59cb\u5316\u79bb\u7ebf\u8bb0\u5fc6\u5e93\u0024')) -VerifySourceStability:$SourceCandidateStability
    $report.offline.action_stage = 'key_generation'
    Press-OfflineButton ([regex]::Unescape('\u005e\u521b\u5efa\u672c\u673a\u5bc6\u94a5\u0024'))
    # Never read arbitrary tree nodes/values, capture a screenshot, copy or archive recovery material.
    $null = Find-OfflineControl 'ControlType.Edit' ([regex]::Unescape('\u005e\u8bf7\u4fdd\u5b58\u672c\u673a\u6062\u590d\u6750\u6599\u0024')) 60
    $report.offline.action_stage = 'recovery_dismiss'
    Press-OfflineButton ([regex]::Unescape('\u005e\u5173\u95ed\u0024')) # Focus the recovery dialog's fixed close button before native input.
    Wait-OfflineRecoveryDismissed
    $report.offline.action_stage = 'overview_navigation'
    Press-OfflineButton ([regex]::Unescape('\u005e\u6982\u89c8\u0024'))
    $report.offline.runtime_binding_stage = 'start_control'
    $report.offline.action_stage = 'runtime_start'
    Press-OfflineButton ([regex]::Unescape('\u005e\u542f\u52a8\u0020\u0052\u0075\u006e\u0074\u0069\u006d\u0065\u0024'))
    $null = Find-OfflineControl 'ControlType.Button' ([regex]::Unescape('\u005e\u505c\u6b62\u0020\u0052\u0075\u006e\u0074\u0069\u006d\u0065\u0024')) 60 -ObserveStartStopSelector
    Bind-OfflineRuntime
    $report.offline.action_stage = 'account_verification'
    Press-OfflineButton ([regex]::Unescape('\u005e\u8d26\u6237\u4e0e\u7a7a\u95f4\u0024'))
    $report.offline.action_stage = 'credential_persistence'
    Assert-OfflineLocalKeys (Read-OfflineRpc @('--json','status'))
    $null = Find-OfflineControl 'ControlType.Text' ([regex]::Unescape('\u005e\u5df2\u4fdd\u5b58\u0024'))
    $report.offline.local_keys_initialized = $true
    $marker = 'gui-offline-' + [Guid]::NewGuid().ToString('N')
    $importantTitle = $marker + '-important'; $importantContent = $marker + ' important body'
    Create-OfflineMemory $importantTitle $importantContent $false
    $list = Read-OfflineRpc @('--json','list','--limit','100')
    if (-not ($list.details -is [Array])) { throw 'Runtime list details is not the original array contract' }
    $matches = @($list.details | ForEach-Object { Convert-OfflineEntry $_ } | Where-Object { $_.title -eq $importantTitle })
    if ($matches.Count -ne 1) { throw 'Important UI write did not produce one real Runtime entry' }
    $show = Read-OfflineRpc @('--json','show',$matches[0].id)
    $entry = Convert-OfflineEntry $show
    if ($entry.id -ne $matches[0].id -or $entry.title -ne $importantTitle -or
        $entry.content -ne $importantContent -or $entry.importance -ne 'important') { throw 'Important UI/Runtime readback mismatch' }
    $report.offline.important_memory_readback = $true
    $report.offline.important_id = $entry.id
    $report.offline.restart_memory = @{id=$entry.id; title=$importantTitle; query=$importantContent}
    Assert-OfflineRecall $entry.id $importantTitle $importantContent
    $diaryContent = $marker + ' diary body'
    Create-OfflineMemory ($marker + '-diary') $diaryContent $true
    $report.offline.action_stage = 'diary_runtime_readback'
    $diary = Read-OfflineRpc @('--json','diary')
    if (-not ($diary.details -is [Array])) { throw 'Runtime diary details is not the original array contract' }
    $days = @($diary.details | ForEach-Object { Convert-OfflineEntry $_ } | Where-Object { $_.importance -eq 'trivial' -and $_.content -like ('*'+$diaryContent+'*') })
    if ($days.Count -ne 1) { throw 'Trivial UI write did not appear exactly once in the real diary' }
    if ($days[0].title -cne ($marker + '-diary')) { throw 'Diary title differs from the explicit synthetic UI write' }
    $report.offline.action_stage = 'diary_navigation'
    Press-OfflineButton (Get-OfflineDiaryNavigationPattern)
    $report.offline.action_stage = 'diary_select_saved_day'
    Press-OfflineButton ('^' + [regex]::Escape($days[0].title) + '$')
    $report.offline.action_stage = 'diary_saved_reader'
    Assert-OfflineDiaryReader $days[0].title $diaryContent
    $report.offline.diary_readback = $true; $report.offline.diary_id = $days[0].id
    $guard = Assert-Target $true
    $report.screenshot = Capture-Target $guard.client_bounds
    $report.offline.screenshot_stage = 'real diary UI after synthetic write and Runtime readback; recovery dialog closed'
    $report.offline.action_stage = 'diary_close_reader'
    Close-OfflineDiaryReader $days[0].title
    $report.offline.action_stage = 'settings_navigation'
    Press-OfflineButton ([regex]::Unescape('\u005e\u8bbe\u7f6e\u0024'))
    $report.offline.settings_inputs = @{}
    Set-OfflineAutoRuntime $true
    if (-not $binding.final_public_assets -and -not $binding.verified_candidate_assets) { Set-OfflineAutoRuntime $false }
    $report.offline.settings_persisted = $true
    $report.offline.action_stage = 'runtime_pre_stop_check'
    $null = Assert-OfflineRuntime
    $report.offline.action_stage = 'overview_navigation'
    Press-OfflineButton ([regex]::Unescape('\u005e\u6982\u89c8\u0024'))
    Press-OfflineButton ([regex]::Unescape('\u005e\u505c\u6b62\u0020\u0052\u0075\u006e\u0074\u0069\u006d\u0065\u0024')) -DiagnosticStage 'normal_stop'
    $report.offline.action_stage = 'normal_stop_process_lookup'
    $process = Get-Process -Id $offlineRuntime.pid -ErrorAction SilentlyContinue
    if ($process) {
        $report.offline.action_stage = 'normal_stop_birth_check'
        if ($process.StartTime.ToUniversalTime().Ticks -ne $offlineRuntime.started_ticks) { $report.offline.failure_kind = 'pid_reused'; throw 'Runtime PID reused before normal-stop observation' }
        $report.offline.action_stage = 'normal_stop_wait_exit'
        if (-not $process.WaitForExit(30000)) { $report.offline.failure_kind = 'exit_timeout'; throw 'Runtime did not exit after visible normal Stop' }
    }
    for ($attempt=0; $attempt -lt 2; $attempt++) {
        if ($attempt -eq 0) { $report.offline.action_stage = 'normal_stop_cim_first' } else { $report.offline.action_stage = 'normal_stop_cim_second' }
        if (Get-CimInstance Win32_Process -Filter "ProcessId=$($offlineRuntime.pid)") { $report.offline.failure_kind = 'pid_present_after_stop'; throw 'Runtime PID still exists or was reused after normal Stop' }
        Pause-Probe
    }
    $report.offline.action_stage = 'normal_stop_confirmed'
    $report.offline.runtime_normal_stop_confirmed = $true
    $report.offline.status = 'offline-subset-passed'
}
