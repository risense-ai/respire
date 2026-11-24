function Stop-OurRuntimes {
    # Runtime command lines contain --runtime-internal; the data directory is in the environment.
    # With -Exe, the binary is outside the npm prefix; locate the isolated runtime correctly.
    $self = $PID
    $targets = New-Object System.Collections.Generic.List[int]
    if (Test-Path -LiteralPath $resolvedRoot) {
        Get-ChildItem -LiteralPath $resolvedRoot -Filter endpoint.json -Recurse -File -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '[\\/]runtime[\\/]endpoint\.json$' } |
            ForEach-Object {
                try {
                    $ep = Get-Content -LiteralPath $_.FullName -Raw -Encoding utf8 | ConvertFrom-Json
                    $procId = 0
                    if ($null -ne $ep.pid) { $procId = [int]$ep.pid }
                    if ($procId -gt 0 -and $procId -ne $self) { [void]$targets.Add($procId) }
                } catch { }
            }
    }
    foreach ($procId in ($targets | Select-Object -Unique)) {
        Stop-Process -Id $procId -Force -ErrorAction SilentlyContinue
    }
    foreach ($procId in ($targets | Select-Object -Unique)) {
        for ($i = 0; $i -lt 25; $i++) {
            if (-not (Get-Process -Id $procId -ErrorAction SilentlyContinue)) { break }
            Start-Sleep -Milliseconds 200
        }
    }
}
$script:CloudAccounts = New-Object System.Collections.Generic.List[object]
$script:CloudCleanup = @{ passed = $false; events = @(); remaining_users = @() }
$script:CloudCleanupCompleted = $false
$script:CloudCleanupRunning = $false
$script:ModelProgressEvidence = $null
$script:IndexPendingEvents = New-Object System.Collections.Generic.List[object]
$script:SweepReportClock = [Diagnostics.Stopwatch]::StartNew()
$script:BusinessStatus = 'in-progress'
trap { $failure = $_; if ($BusinessOnly) { $script:BusinessStatus = 'failed' }; Invoke-CloudCleanup; Write-SweepReport -Force; Stop-OurRuntimes; throw $failure }
$Rows = New-Object System.Collections.Generic.List[object]
$script:Step = 0
$script:PublishedRuntimePaths = @{}
$script:RpcTokens = @{}

function Write-SweepReport([switch]$Force) {
    # Keep periodic diagnostics without rebuilding the entire report after every assertion.
    if (-not $Force -and $script:SweepReportClock.Elapsed.TotalSeconds -lt 5) { return }
    $lines = @('# CLI development sweep', '', "- Server: $Server", "- Version: $ExpectVersion", '', '| Step | Command | Exit | Status | Result | Note |', '| --- | --- | --- | --- | --- | --- |')
    foreach ($row in $Rows) {
        $mark = if (-not $row.Ok) { 'FAIL' } elseif ($row.Status -eq 'fail') { 'EXPECTED_ERROR' } elseif ($row.Status -eq 'skip') { 'SKIP' } else { 'PASS' }
        $note = Get-SafeReportNote (($row.Note + ' ' + $row.Leak).Trim())
        $lines += "| $($row.Step) | $($row.Name) | $($row.Exit) | $($row.Status) | $mark | $note |"
    }
    [IO.File]::WriteAllText($ReportPath, ($lines -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
    # Deliberately omit command output, arguments, notes, and credential material.
    $observed = @($Rows | ForEach-Object {
        @{ case = $_.Name; command = $_.Command; step = $_.Step; exit = $_.Exit; status = $_.Status; result = $(if (-not $_.Ok) { 'failed' } elseif ($_.Status -eq 'skip') { 'skipped' } elseif ($_.Status -eq 'fail') { 'expected_error_contract' } else { 'passed' })
            account_register_vault_pending = $_.account_register_vault_pending
            account_register_vault_published = $_.account_register_vault_published }
    })
    $coverage = @{
        schema_version = 1; server = $Server; source_sha = $env:CLI_SHA; workflow_sha = $env:GITHUB_SHA
        expected_version = $ExpectVersion; observed_version = $script:ObservedVersion
        observed_version_display = $script:ObservedVersionDisplay
        cloud_cleanup = $script:CloudCleanup
        model_progress = $script:ModelProgressEvidence
        index_pending_events = @($script:IndexPendingEvents.ToArray())
        binary_sha256 = $script:BinarySha256; catalog = $script:Catalog
        cases = $observed; passed = @($Rows | Where-Object { $_.Ok -and $_.Status -notin @('skip', 'fail') }).Count
        expected_errors = @($Rows | Where-Object { $_.Ok -and $_.Status -eq 'fail' }).Count
        failed = @($Rows | Where-Object { -not $_.Ok }).Count
        skipped = @($Rows | Where-Object { $_.Status -eq 'skip' }).Count
        required_missing = @($script:Catalog.required_assertions | Where-Object { $required = $_; -not @($Rows | Where-Object { $_.Name -ceq $required -and $_.Ok -and $_.Status -ceq 'ok' }).Count })
        base_complete = @($Rows | Where-Object { -not $_.Ok }).Count -eq 0 -and @($script:Catalog.required_assertions | Where-Object { $required = $_; -not @($Rows | Where-Object { $_.Name -ceq $required -and $_.Ok -and $_.Status -ceq 'ok' }).Count }).Count -eq 0
        supplemental_required = @($script:Catalog.supplemental_required)
        supplemental_deferred = [bool]$DeferSupplemental
        coverage_full = @($script:Catalog.required_remaining).Count -eq 0 -and @($script:Catalog.supplemental_required).Count -eq 0 -and @($Rows | Where-Object { -not $_.Ok }).Count -eq 0 -and @($script:Catalog.required_assertions | Where-Object { $required = $_; -not @($Rows | Where-Object { $_.Name -ceq $required -and $_.Ok -and $_.Status -ceq 'ok' }).Count }).Count -eq 0
    }
    if ($BusinessOnly) {
        $coverage.diagnostic_only = $true
        $coverage.publication_reusable = $false
        $coverage.full_acceptance = $false
        $coverage.release_acceptance = $false
        $coverage.business_status = $script:BusinessStatus
        $coverage.not_evaluated = @('other_cli_checks', 'supplemental_suites', 'api-smoke', 'publication_acceptance')
        $coverage.base_complete = $false
        $coverage.coverage_full = $false
    }
    [IO.File]::WriteAllText($CoveragePath, ($coverage | ConvertTo-Json -Depth 20) + "`n", [Text.UTF8Encoding]::new($false))
    $script:SweepReportClock.Restart()
}

function Get-SafeReportNote([string]$Text) {
    $Text = [regex]::Replace($Text, '(?i)(Bearer\s+)[a-z0-9._-]+', '${1}[redacted]')
    $Text = [regex]::Replace($Text, '(?i)([?&]token=)[^&\s|]+', '${1}[redacted]')
    $Text = [regex]::Replace($Text, '(?i)("(?:token|super|secret_key|pass|password|wrapped_urk|kdf_salt|urk_nonce)"\s*:\s*")[^"]*', '${1}[redacted]')
    return $Text -replace '\|', '/'
}

function Get-SafeFailureReason($Envelope) {
    # These structured account outcomes survive shared-host error wrapping.
    # Require a scalar string and emit constants, never stringify/copy private data.
    $reason = $Envelope.summary.reason
    if ($reason -is [string]) {
        if ($reason -ceq 'account_outcome_unknown') {
            foreach ($class in @('register_remote_unknown', 'register_native_credentials', 'register_state_conflict',
                'register_material_invalid', 'register_local_compatibility', 'register_unclassified')) {
                if (@($Envelope.errors | Where-Object { $_ -is [string] -and $_.EndsWith("; register_kind=$class", [StringComparison]::Ordinal) }).Count -gt 0) {
                    return "account_outcome_unknown/$class"
                }
            }
            foreach ($class in @('vault_unlock_rejected', 'account_material_rejected',
                'auth_salt_http_unauthorized', 'auth_salt_http_forbidden', 'auth_salt_http_rate_limited', 'auth_salt_http_server_error', 'auth_salt_http_other', 'auth_salt_transport',
                'auth_login_http_unauthorized', 'auth_login_http_forbidden', 'auth_login_http_rate_limited', 'auth_login_http_server_error', 'auth_login_http_other', 'auth_login_transport',
                'auth_salt_missing', 'auth_salt_invalid_hex', 'auth_login_reply_parse_failed',
                'auth_totp_ticket_missing', 'auth_totp_ticket_invalid', 'auth_login_token_missing')) {
                if (@($Envelope.errors | Where-Object { $_ -is [string] -and $_.EndsWith("; login_kind=$class", [StringComparison]::Ordinal) }).Count -gt 0) {
                    return "account_outcome_unknown/$class"
                }
            }
            # Match only fixed source markers within the original account error chain.
            foreach ($entry in @(
                @('owned account Runtime exited before readiness', 'account_candidate_exited_before_ready'),
                @('owned account Runtime did not become ready; query the original operation', 'account_candidate_readiness_timeout'),
                @('Runtime owner changed during account startup', 'account_candidate_owner_mismatch'),
                @('Runtime process changed during account startup verification', 'account_candidate_process_changed'),
                @('account target launch does not belong to this maintenance controller', 'account_candidate_launch_binding_mismatch'),
                @('starting Runtime exited', 'account_candidate_exited_during_verification'),
                @('selected Runtime port differs from host transport', 'account_candidate_port_mismatch'),
                @('failed to stop owned unready Runtime', 'account_candidate_stop_failed'),
                @('failed to reap owned unready Runtime', 'account_candidate_reap_failed')
            )) {
                if (@($Envelope.errors | Where-Object { $_ -is [string] -and
                    ($_ -ceq $entry[0] -or $_.EndsWith(": $($entry[0])", [StringComparison]::Ordinal) -or
                        $_.Contains(": $($entry[0]): ", [StringComparison]::Ordinal)) }).Count -gt 0) {
                    return "account_outcome_unknown/$($entry[1])"
                }
            }
            # Fixed classes only; preserve the original unknown outcome and never replay it.
            foreach ($entry in @(
                @('parent authorization ended or control channel was interrupted', 'account_parent_revoked_or_channel_interrupted'),
                # Explicit versions can still select historical CLI artifacts.
                @('account deadline or parent authorization ended|control deadline or parent revocation; query the same operation ID before retrying', 'account_runtime_deadline_or_parent_revoked'),
                @('Runtime rejected account control \(unauthorized\)', 'account_launch_or_parent_authorization_failed'),
                @('Runtime rejected account control \(scope_mismatch\)', 'account_control_binding_mismatch'),
                @('Runtime rejected account control \(auth_ownership_unavailable\)', 'account_library_ownership_unavailable'),
                @('Runtime rejected account control \(control_unavailable\)', 'account_controller_unavailable'),
                @('cannot start owned host control Runtime', 'account_helper_start_failed'),
                @('missing host control input pipe|missing host control output pipe|host control input owner was released|host control request write failed|host control response channel failure', 'account_helper_channel_failed'),
                @('cannot wait for host control Runtime exit', 'account_helper_exit_wait_failed'),
                @('host control Runtime exit does not match its response', 'account_helper_exit_mismatch'),
                @('host control deadline exceeded', 'account_helper_launch_deadline'),
                @('host control response deadline or channel failure|host control response deadline exceeded|host control exit deadline exceeded', 'account_helper_deadline_or_channel'),
                @('missing host control response frame|incomplete host control response frame|invalid host control response frame size|invalid host control response JSON|extra host control response bytes', 'account_helper_frame_invalid'),
                @('account control response identity mismatch', 'account_rpc_identity_mismatch'),
                @('account response launch profile mismatch|account receipt operation or profile mismatch', 'account_profile_binding_mismatch'),
                @('account control rejection envelope missing|invalid account control rejection code|invalid account control rejection envelope|account control result is missing|invalid account control envelope|invalid account receipt shape|account control outcome/status mismatch', 'account_response_invalid'),
                @('account helper readiness was not confirmed', 'account_response_readiness_invalid'),
                @('account target Runtime is unavailable|target Runtime does not match the selected account, image or managed owner|target Runtime or account identity changed during verification|account target launch does not belong to this maintenance controller', 'account_target_verification_failed')
            )) {
                if (@($Envelope.errors | Where-Object { $_ -is [string] -and $_ -match $entry[0] }).Count -gt 0) {
                    return "account_outcome_unknown/$($entry[1])"
                }
            }
            if (@($Envelope.errors | Where-Object { $_ -is [string] -and $_.EndsWith('Runtime rejected account control (account_outcome_unknown): account outcome is unknown; query the original operation without replaying', [StringComparison]::Ordinal) }).Count -gt 0) {
                return 'account_outcome_unknown/account_runtime_rejected'
            }
            return 'account_outcome_unknown'
        }
        if ($reason -ceq 'account_handoff_pending') { return 'account_handoff_pending' }
    }
    # Report fixed classifications only, never raw authentication responses.
    $message = @($Envelope.errors | Where-Object { $_ -is [string] }) -join ' '
    if ($Envelope.command -is [string] -and $Envelope.command -ceq 'space' -and $Envelope.summary.outcome -is [string]) {
        switch -CaseSensitive ($Envelope.summary.outcome) {
            'unknown' { return 'space_outcome_unknown' }
            'pending' { return 'space_pending' }
            'rejected' { return 'space_rejected' }
        }
    }
    foreach ($code in @('unauthorized', 'scope_mismatch', 'auth_ownership_unavailable', 'control_unavailable', 'space_outcome_unknown', 'control_outcome_unknown')) {
        if ($message -cmatch "Runtime rejected space control \($code\);") { return "space_runtime_$code" }
    }
    foreach ($entry in @(
        @('space response binding mismatch|space response profile mismatch|space receipt differs from its original identity or intent', 'space_response_binding_mismatch'),
        @('space control rejection envelope missing|invalid space control rejection code|invalid space control rejection envelope|space receipt missing|invalid space envelope|invalid space receipt|invalid space write receipt|invalid committed space outcome|invalid pending space outcome|invalid unknown space outcome|invalid rejected space outcome', 'space_response_invalid'),
        @('space readiness mismatch', 'space_response_readiness_invalid'),
        @('join target receipt mismatch|join account receipt mismatch|join session digest mismatch|join readonly_team gate mismatch|join unexpectedly changed the agent gate|pending join claims completion', 'space_join_receipt_invalid'),
        @('cannot start owned host control Runtime', 'auth_helper_start_failed'),
        @('missing host control input pipe|missing host control output pipe|host control input owner was released|host control request write failed|host control response channel failure', 'auth_helper_channel_failed'),
        @('cannot wait for host control Runtime exit', 'auth_helper_exit_wait_failed'),
        @('host control Runtime exit does not match its response', 'auth_helper_exit_mismatch'),
        @('control deadline or parent revocation; query the same operation ID before retrying', 'auth_runtime_deadline_or_parent_revoked')
    )) {
        if ($message -match $entry[0]) { return $entry[1] }
    }
    if ($message -match '\b([45]\d\d)\b') { return "http_$($Matches[1])" }
    foreach ($entry in @(
        @('auth control requires an installed managed Runtime', 'auth_runtime_missing'),
        @('auth control Runtime version or image does not match the managed manifest', 'auth_runtime_image_mismatch'),
        @('auth_diagnostic=auth_parent_revoked_or_channel_interrupted\b', 'auth_parent_revoked_or_channel_interrupted'),
        # Retain fixed diagnostics from explicitly selected historical CLI versions.
        @('auth_diagnostic=auth_runtime_deadline_or_parent_revoked\b', 'auth_runtime_deadline_or_parent_revoked'),
        @('auth_diagnostic=auth_credential_backend_unavailable\b', 'auth_credential_backend_unavailable'),
        @('auth_diagnostic=auth_credential_backend_not_persistent\b', 'auth_credential_backend_not_persistent'),
        @('auth_diagnostic=auth_credential_write_readback_failed\b', 'auth_credential_write_readback_failed'),
        @('host control deadline exceeded', 'auth_helper_launch_deadline'),
        @('host control response deadline or channel failure|host control response deadline exceeded|host control exit deadline exceeded', 'auth_helper_deadline_or_channel'),
        @('missing host control response frame|incomplete host control response frame|invalid host control response frame size|invalid host control response JSON|extra host control response bytes', 'auth_helper_frame_invalid'),
        @('auth control response identity mismatch|host control response identity mismatch', 'auth_response_identity_mismatch'),
        @('auth control response profile mismatch', 'auth_response_profile_mismatch'),
        @('auth control readiness was not confirmed', 'auth_response_readiness_invalid'),
        @('invalid auth control result shape|auth control result is missing|unexpected auth control command|auth control result owner mismatch|committed auth result is missing the recovery code', 'auth_result_invalid'),
        @('already registered', 'account_exists'),
        @('runtime owns its boot profile', 'profile_change_requires_host'),
        @('read.only', 'readonly_local_guard'),
        @('none in the local keyring|super password required', 'recovery_key_missing'),
        @('keyring write did not round-trip', 'keyring_readback_failed'),
        @('local keyring write failed|keyring write failed', 'keyring_write_failed'),
        @('keyring|keychain', 'keyring'),
        @('session not unlocked|vault.*locked', 'vault_locked'),
        @('TLS|certificate', 'tls'),
        @('DNS|resolve', 'dns'),
        @('connect|timeout|transport', 'transport'),
        @('runtime.*token|runtime.*auth', 'runtime_auth'),
        @('space_recovery_required operation_id=', 'space_recovery_required'),
        @('auth_outcome_unknown operation_id=|auth control completion is unknown; query the original operation', 'auth_outcome_unknown')
    )) {
        if ($message -match $entry[0]) { return $entry[1] }
    }
    return 'unclassified'
}

function Get-SafeAccountPhase($Envelope, [string]$DataDir, [string]$FixtureRoot, [hashtable]$RegisterState = @{}) {
    $RegisterState.account_register_vault_pending = $null
    $RegisterState.account_register_vault_published = $null
    if ($Envelope.summary.reason -isnot [string] -or $Envelope.summary.reason -cne 'account_outcome_unknown') { return '' }
    $unavailable = 'account_phase=phase_unavailable'
    try {
        $operation = $Envelope.details.operation_id
        $source = $Envelope.details.source_profile
        if ($operation -isnot [string] -or $operation -cnotmatch '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$' -or $source -isnot [string]) { return $unavailable }
        $normalize = {
            param([string]$Value)
            if (-not [IO.Path]::IsPathFullyQualified($Value)) { throw 'invalid_fixture_path' }
            if ($IsWindows) { $Value = $Value -replace '^\\\\\?\\', '' }
            return [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath($Value))
        }
        $fixturePath = & $normalize $FixtureRoot
        $devicePath = & $normalize $DataDir
        $sourcePath = & $normalize $source
        $comparison = if ($IsWindows) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
        if ((-not $devicePath.Equals($fixturePath, $comparison) -and -not $devicePath.StartsWith($fixturePath + [IO.Path]::DirectorySeparatorChar, $comparison)) -or
            (-not $sourcePath.Equals($devicePath, $comparison) -and -not $sourcePath.StartsWith($devicePath + [IO.Path]::DirectorySeparatorChar, $comparison))) { return $unavailable }
        $journalPath = Join-Path $devicePath '.login-recovery.json'
        foreach ($path in @($sourcePath, $journalPath)) {
            for ($ancestor = $path; $ancestor; $ancestor = [IO.Path]::GetDirectoryName($ancestor)) {
                if (([IO.File]::GetAttributes($ancestor) -band [IO.FileAttributes]::ReparsePoint) -ne 0) { return $unavailable }
            }
        }
        if (([IO.File]::GetAttributes($sourcePath) -band [IO.FileAttributes]::Directory) -eq 0) { return $unavailable }
        $journal = [IO.File]::ReadAllText($journalPath) | ConvertFrom-Json -ErrorAction Stop
        if ($journal -isnot [pscustomobject] -or $journal.schema_version -isnot [long] -or $journal.schema_version -ne 2 -or $journal.operations -isnot [array] -or $journal.active -isnot [pscustomobject] -or
            $journal.active.operation_id -isnot [string] -or $journal.active.operation_id -cne $operation) { return $unavailable }
        $records = @($journal.operations | Where-Object { $_.operation_id -is [string] -and $_.operation_id -eq $operation })
        if ($records.Count -ne 1) { return $unavailable }
        $record = $records[0]
        if ($record -isnot [pscustomobject] -or $record.operation_id -cne $operation -or $record.idempotency_key -isnot [string] -or $record.idempotency_key -cne $operation -or
            $record.root -isnot [string] -or $record.profile -isnot [string] -or $journal.active.directory -isnot [string] -or
            $record.alias -isnot [string] -or -not $record.alias -or $journal.active.alias -isnot [string] -or $journal.active.alias -cne $record.alias -or
            $record.account -isnot [pscustomobject]) { return $unavailable }
        $activePath = $sourcePath
        if ($record.account.target -is [string] -and $record.account.target.Length -gt 0) {
            $activePath = & $normalize $record.account.target
            if (-not $activePath.Equals($devicePath, $comparison) -and
                -not $activePath.StartsWith($devicePath + [IO.Path]::DirectorySeparatorChar, $comparison)) { return $unavailable }
        }
        if (-not (& $normalize $record.root).Equals($devicePath, $comparison) -or
            -not (& $normalize $record.profile).Equals($sourcePath, $comparison) -or
            -not (& $normalize $journal.active.directory).Equals($activePath, $comparison)) { return $unavailable }
        $phase = $record.phase
        if ($phase -isnot [string] -or $phase -cnotin @('admitted', 'login_input', 'remote_pending', 'remote_unknown', 'authorized',
            'prepared', 'awaiting_verification', 'rolling_back', 'committed', 'rolled_back')) { return $unavailable }
        $pending = ''
        if ($null -ne $record.account.login) {
            if ($record.account.login -isnot [pscustomobject] -or $record.account.login.publication_pending -isnot [bool]) { return $unavailable }
            $pending = if ($record.account.login.publication_pending) { ' pubpending=true' } else { ' pubpending=false' }
        }
        $registerDiagnostic = ''
        if ($record.account.register -is [bool] -and $record.account.register) {
            foreach ($field in @(@('vault_pending', 'account_register_vault_pending'), @('published_vault', 'account_register_vault_published'))) {
                $value = $record.account.($field[0])
                if ($value -is [bool]) { $RegisterState[$field[1]] = $value }
                $fixed = if ($value -is [bool]) { $value.ToString().ToLowerInvariant() } else { 'null' }
                $registerDiagnostic += " $($field[1])=$fixed"
            }
        }
        return "account_phase=$phase$pending$registerDiagnostic"
    } catch { return $unavailable }
}

function Assert-DevEnvelope($Envelope, [string]$Where) {
    if (-not $Envelope -or -not $Envelope.summary) { return }
    foreach ($key in @('addr', 'server_addr')) {
        $prop = $Envelope.summary.PSObject.Properties[$key]
        if ($prop -and [string]$prop.Value -and [string]$prop.Value -ne $Server) {
            throw "$Where has a $key outside the approved development server. Sweep stopped."
        }
    }
}

function Get-JsonRpc([string]$Text, $Id) {
    foreach ($line in ($Text -split "`n")) {
        $t = $line.Trim()
        if (-not $t.StartsWith('{')) { continue }
        try {
            $obj = $t | ConvertFrom-Json
            if ($null -ne $obj.id -and [string]$obj.id -eq [string]$Id) { return $obj }
        } catch { }
    }
    return $null
}

function Get-McpToolEnvelope($Rpc) {
    if (-not $Rpc -or -not $Rpc.result) { return $null }
    if ($Rpc.result.isError -eq $true) { return $null }
    $content = $Rpc.result.content
    if (-not $content -or $content.Count -lt 1) { return $null }
    $text = [string]$content[0].text
    try { return $text | ConvertFrom-Json } catch { return $null }
}

function Add-SweepRow([string]$Name, [bool]$Ok, [string]$Note) {
    $script:Step++
    $Rows.Add([pscustomobject]@{
        Step = $script:Step; Name = $Name; Exit = $(if ($Ok) { 0 } else { 1 }); Status = $(if ($Ok) { 'ok' } else { 'fail' }); Ok = $Ok; Leak = ''; Note = $Note
    }) | Out-Null
    Write-Host "$(if ($Ok) {'PASS'} else {'FAIL'}) $Name $Note"
}

function Assert-Smoke([string]$Name, [bool]$Condition) {
    Add-SweepRow $Name $Condition 'semantic assertion'
    Write-SweepReport -Force:(-not $Condition)
    if (-not $Condition) { throw "Release smoke assertion failed: $Name" }
}

function Invoke-McpStdioTool {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Tool,
        [hashtable]$Arguments = @{},
        [Parameter(Mandatory)][string]$DataDir,
        [switch]$AllowDestructiveMcpFixture,
        [int]$TimeoutSec = 90
    )
    $argJson = if ($Arguments.Count -eq 0) { '{}' } else { $Arguments | ConvertTo-Json -Compress -Depth 8 }
    $stdin = (@(
        '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"sweep","version":"0"}}}'
        '{"jsonrpc":"2.0","method":"notifications/initialized"}'
        "{`"jsonrpc`":`"2.0`",`"id`":2,`"method`":`"tools/call`",`"params`":{`"name`":`"$Tool`",`"arguments`":$argJson}}"
    ) -join "`n") + "`n"
    $run = Invoke-Om -Name $Name -ArgList @('mcp') -DataDir $DataDir -Raw -TimeoutSec $TimeoutSec -StdinText $stdin -Note $Tool -AllowDestructiveMcpFixture:$AllowDestructiveMcpFixture
    $rpc = Get-JsonRpc $run.Stdout 2
    $envlp = Get-McpToolEnvelope $rpc
    return [pscustomobject]@{ Run = $run; Rpc = $rpc; Envelope = $envlp }
}

function Get-Envelope([string]$Text) {
    $t = $Text.Trim()
    if (-not $t) { return $null }
    $start = $t.IndexOf("{")
    if ($start -lt 0) { return $null }
    $slice = $t.Substring($start)
    try { return $slice | ConvertFrom-Json } catch { return $null }
}

function Get-Leak([string]$Text, [bool]$Secret) {
    $hits = @()
    if ($Text -cmatch 'DETAILS') { $hits += 'DETAILS' }
    if (-not $Secret) {
        foreach ($w in @('"ciphertext"', '"embedding_enc"', '"wrapped_urk"', '"kdf_salt"', '"embedding"', '"nonce"')) {
            if ($Text.Contains($w)) { $hits += $w }
        }
    }
    return $hits
}

# Only the endpoint and token beneath the current disposable device are used.
