Invoke-Om -Name 'show-b' -ArgList @('--json', 'show', $short) -DataDir $DirB | Out-Null
$bad = Join-Path $Root 'bad'
Invoke-Om -Name 'config-addr-bad' -ArgList @('--json', 'config', '--addr', $Server) -DataDir $bad | Out-Null
Invoke-Om -Name 'login-wrong-pass' -ArgList @('--json', 'login', '--interactive', '--addr', $Server, '--user', $user, '--pass', 'wrong-pass-000') -DataDir $bad -AllowStatus @('fail') -Secret -HostProfile -Note 'a wrong passphrase must fail' | Out-Null

# The second device uses fivekeys. Read material from device A without printing it.
$sessionPath = Join-Path $DirA 'session.json'
if (-not (Test-Path $sessionPath)) {
    $sessionPath = Get-ChildItem -Path $DirA -Filter session.json -Recurse | Select-Object -First 1 -ExpandProperty FullName
}
if ($sessionPath -and (Test-Path $sessionPath)) {
    $fivekeysConfig = Invoke-Om -Name 'config-addr-d' -ArgList @('--json', 'config', '--addr', $Server) -DataDir $DirD
    $sess = Get-Content $sessionPath -Raw -Encoding utf8 | ConvertFrom-Json
    $fivekeysSelected = $fivekeysConfig.Envelope.summary.data_dir
    $fivekeysNote = 'user_present={0} pass_present={1} secret_present={2} kdf_salt_present={3} wrapped_urk_present={4} urk_nonce_present={5} super_present={6} selected_profile_observed={7} selected_profile_matches={8}' -f `
        (-not [string]::IsNullOrEmpty($user)), (-not [string]::IsNullOrEmpty($pass)), (-not [string]::IsNullOrEmpty([string]$sess.secret_key)), `
        (-not [string]::IsNullOrEmpty([string]$sess.kdf_salt)), (-not [string]::IsNullOrEmpty([string]$sess.wrapped_urk)), `
        (-not [string]::IsNullOrEmpty([string]$sess.urk_nonce)), (-not [string]::IsNullOrEmpty($super)), `
        ($fivekeysSelected -is [string] -and -not [string]::IsNullOrEmpty($fivekeysSelected)), `
        ($fivekeysSelected -is [string] -and $fivekeysSelected -eq [IO.Path]::GetFullPath($DirD))
    $fivekeysResult = Invoke-Om -Name 'fivekeys-d' -ArgList @(
        '--json', 'fivekeys',
        '--addr', $Server,
        '--user', $user,
        '--pass', $pass,
        '--secret', ([string]$sess.secret_key),
        '--kdf-salt', ([string]$sess.kdf_salt),
        '--wrapped-urk', ([string]$sess.wrapped_urk),
        '--urk-nonce', ([string]$sess.urk_nonce),
        '--super', $super
    ) -DataDir $DirD -Secret -TimeoutSec 180 -Note $fivekeysNote
    if (-not $fivekeysResult.Ok) {
        $fivekeysReason = 'unclassified'
        foreach ($class in @('key_outcome_unknown', 'auth_outcome_unknown', 'identity_write_recovery_required', 'invalid_input', 'runtime_error')) {
            if ($fivekeysResult.Envelope.summary.reason -is [string] -and $fivekeysResult.Envelope.summary.reason -ceq $class) { $fivekeysReason = $class; break }
        }
        $fivekeysResult.Row.Note += " fivekeys_reason=$fivekeysReason"
        Write-SweepReport -Force
    }
}

Invoke-Om -Name 'config-autosync-off-a' -ArgList @('--json', 'config', '--autosync', 'false') -DataDir $DirA | Out-Null
Invoke-Om -Name 'config-autosync-off-b' -ArgList @('--json', 'config', '--autosync', 'false') -DataDir $DirB | Out-Null
Invoke-Om -Name 'update-a-offline' -ArgList @('--json', 'update', $short, '--content', "conflict-a $marker") -DataDir $DirA -TimeoutSec 180 | Out-Null
Invoke-Om -Name 'update-b-offline' -ArgList @('--json', 'update', $short, '--content', "conflict-b $marker") -DataDir $DirB -TimeoutSec 180 | Out-Null
Invoke-Om -Name 'sync-a-after-edit' -ArgList @('--json', 'sync') -DataDir $DirA -TimeoutSec 180 | Out-Null
Invoke-Om -Name 'sync-b-conflict' -ArgList @('--json', 'sync') -DataDir $DirB -TimeoutSec 180 | Out-Null
$conf = Invoke-Om -Name 'sync-conflicts-after' -ArgList @('--json', 'sync-conflicts', '--refresh') -DataDir $DirB -TimeoutSec 180
$epoch = ''
$rev = 0
$head = 0
$conflict = $null
if ($conf.Envelope.details -is [System.Array] -and $conf.Envelope.details.Count -gt 0) {
    $conflict = $conf.Envelope.details[0]
} elseif ($conf.Envelope.details -and $conf.Envelope.details.epoch) {
    $conflict = $conf.Envelope.details
}
if ($conflict) {
    $epoch = [string]$conflict.epoch
    $rev = [int]$conflict.rev
    $head = [int]$conflict.current.rev
}
if ($epoch) {
    $resolved = Invoke-Om -Name 'sync-resolve' -ArgList @(
        '--json', 'sync-resolve', '--epoch', $epoch, '--rev', "$rev", '--head-rev', "$head",
        '--action', 'merge', '--content', "merged $marker"
    ) -DataDir $DirB -TimeoutSec 180
    $resolvedRead = Invoke-Om -Name 'sync-resolve-merge-readback' -ArgList @('--json', 'show', $short) -DataDir $DirB
    Assert-Smoke 'sync-resolve-merge-content-persisted' ($resolved.Ok -and $resolvedRead.Ok -and [string]$resolvedRead.Envelope.details.entry.content -eq "merged $marker")
} else {
    Assert-Smoke 'sync-resolve-merge-content-persisted' $false
}

foreach ($resolveAction in @('keep-current', 'take-incoming', 'merge')) {
    $resolveName = "resolve-$resolveAction"
    $resolveCreate = Invoke-Om -Name "$resolveName-create" -ArgList @('--json', 'remember', "$resolveName original $stamp", '--title', "$resolveName-$stamp", '--parent', $id, '--importance', 'important', '--force') -DataDir $DirA
    $resolveId = [string]$resolveCreate.Envelope.summary.id
    Assert-Smoke "$resolveName-fixture-id" ($resolveCreate.Ok -and [bool]$resolveId)
    Invoke-Om -Name "$resolveName-baseline-upload" -ArgList @('--json', 'sync') -DataDir $DirA | Out-Null
    Invoke-Om -Name "$resolveName-baseline-download" -ArgList @('--json', 'sync') -DataDir $DirB | Out-Null
    Invoke-Om -Name "$resolveName-edit-a" -ArgList @('--json', 'update', $resolveId, '--content', "$resolveName current $stamp") -DataDir $DirA | Out-Null
    Invoke-Om -Name "$resolveName-edit-b" -ArgList @('--json', 'update', $resolveId, '--content', "$resolveName incoming $stamp") -DataDir $DirB | Out-Null
    Invoke-Om -Name "$resolveName-upload-a" -ArgList @('--json', 'sync') -DataDir $DirA | Out-Null
    Invoke-Om -Name "$resolveName-upload-b" -ArgList @('--json', 'sync') -DataDir $DirB | Out-Null
    $resolveConflicts = Invoke-Om -Name "$resolveName-conflict" -ArgList @('--json', 'sync-conflicts', '--id', $resolveId, '--refresh') -DataDir $DirB
    if (-not $resolveConflicts.Ok) { throw "Release smoke conflict refresh failed: $resolveName-conflict" }
    $resolveCandidates = @($resolveConflicts.Envelope.details | Where-Object { [string]$_.id -eq $resolveId -and [string]$_.state -eq 'pending' })
    Assert-Smoke "$resolveName-distinct-conflict" ($resolveCandidates.Count -eq 1 -and [string]$resolveCandidates[0].candidate.entry.content -ne [string]$resolveCandidates[0].current.value.entry.content)
    $candidate = $resolveCandidates[0]
    $expectedContent = switch ($resolveAction) {
        'keep-current' { [string]$candidate.current.value.entry.content }
        'take-incoming' { [string]$candidate.candidate.entry.content }
        'merge' { "$resolveName merged $stamp" }
    }
    $resolveArgs = @('--json', 'sync-resolve', '--epoch', ([string]$candidate.epoch), '--rev', ([string]$candidate.rev), '--head-rev', ([string]$candidate.current.rev), '--action', $resolveAction)
    if ($resolveAction -eq 'merge') { $resolveArgs += @('--content', $expectedContent) }
    $resolveApply = Invoke-Om -Name "$resolveName-apply" -ArgList $resolveArgs -DataDir $DirB
    Assert-Smoke "$resolveName-acknowledged" ($resolveApply.Ok -and $resolveApply.Envelope.summary.processed -eq $true -and [string]$resolveApply.Envelope.summary.action -eq $resolveAction.Replace('-', '_'))
    Invoke-Om -Name "$resolveName-converge-a" -ArgList @('--json', 'sync') -DataDir $DirA | Out-Null
    $resolveReadA = Invoke-Om -Name "$resolveName-readback-a" -ArgList @('--json', 'show', $resolveId) -DataDir $DirA
    $resolveReadB = Invoke-Om -Name "$resolveName-readback-b" -ArgList @('--json', 'show', $resolveId) -DataDir $DirB
    Assert-Smoke "$resolveName-content-converged" ($resolveReadA.Ok -and $resolveReadB.Ok -and [string]$resolveReadA.Envelope.details.entry.content -eq $expectedContent -and [string]$resolveReadB.Envelope.details.entry.content -eq $expectedContent)
    $processedConflicts = Invoke-Om -Name "$resolveName-processed-history" -ArgList @('--json', 'sync-conflicts', '--id', $resolveId, '--all') -DataDir $DirB
    Assert-Smoke "$resolveName-history-kept" ($processedConflicts.Ok -and @($processedConflicts.Envelope.details | Where-Object { $_.rev -eq $candidate.rev -and [string]$_.state -eq 'processed' -and [string]$_.resolution -eq $resolveAction.Replace('-', '_') }).Count -eq 1)
}

if ($dispId) {
    Invoke-Om -Name 'forget' -ArgList @('--json', 'forget', $dispId) -DataDir $DirA | Out-Null
    $forgotten = Invoke-Om -Name 'forget-show-refused' -ArgList @('--json', 'show', $dispId) -DataDir $DirA -AllowStatus @('fail')
    Assert-Smoke 'forget-hides-entry' ($forgotten.Ok -and $forgotten.Exit -eq 1)
    Invoke-Om -Name 'restore' -ArgList @('--json', 'restore', $dispId) -DataDir $DirA | Out-Null
    $restored = Invoke-Om -Name 'restore-show-visible' -ArgList @('--json', 'show', $dispId) -DataDir $DirA
    Assert-Smoke 'restore-preserves-content' ($restored.Ok -and [string]$restored.Envelope.details.entry.content -eq "disposable $marker")
    Invoke-Om -Name 'purge' -ArgList @('--json', 'purge', $dispId) -DataDir $DirA | Out-Null
    $purged = Invoke-Om -Name 'purge-show-refused' -ArgList @('--json', 'show', $dispId) -DataDir $DirA -AllowStatus @('fail')
    Assert-Smoke 'purge-removes-current-entry' ($purged.Ok -and $purged.Exit -eq 1)
}
$deviceBSession = Get-SmokeSession $DirB $user
$deviceBSid = [string]$deviceBSession.Value.session_id
Assert-Smoke 'session-secondary-id-present' ($deviceBSid -match '^[0-9a-f-]{36}$')
$realSessionList = Invoke-Om -Name 'session-list-secondary-real' -ArgList @('--json', 'session', 'list') -DataDir $DirA
Assert-Smoke 'session-secondary-listed' ($realSessionList.Ok -and ($realSessionList.Envelope.details | ConvertTo-Json -Depth 8) -like "*$deviceBSid*")
$realRevoke = Invoke-Om -Name 'session-revoke-secondary-real' -ArgList @('--json', 'session', 'revoke', $deviceBSid) -DataDir $DirA
Assert-Smoke 'session-revoke-secondary-confirmed' ($realRevoke.Ok -and $realRevoke.Envelope.details.revoked -eq $true -and [string]$realRevoke.Envelope.details.session_id -eq $deviceBSid)
$revokedSync = Invoke-Om -Name 'session-revoked-cloud-sync-denied' -ArgList @('--json', 'sync') -DataDir $DirB -AllowStatus @('fail')
Assert-Smoke 'session-revoked-access-denied' ($revokedSync.Ok -and $revokedSync.Exit -eq 1 -and ($revokedSync.Envelope.errors -join ' ') -match 'unauthorized|token rejected|401')
$revokedLocal = Invoke-Om -Name 'session-revoked-local-read-preserved' -ArgList @('--json', 'show', $id) -DataDir $DirB
Assert-Smoke 'session-revocation-preserves-local-decryption' ($revokedLocal.Ok -and [string]$revokedLocal.Envelope.details.entry.content -eq "merged $marker")
# A revoked owner session is marked personal read-only by the remote transport.
# Recover through the existing CLI setting, while team members remain unable to self-unlock.
$revokedConfig = Invoke-Om -Name 'session-revoked-agent-config' -ArgList @('--json', 'agent-config') -DataDir $DirB
Assert-Smoke 'session-revoked-personal-readonly-marked' ($revokedConfig.Ok -and $revokedConfig.Envelope.summary.readonly -eq $true -and $revokedConfig.Envelope.summary.readonly_team -ne $true)
$revokedStatus = Invoke-Om -Name 'session-revoked-device-status' -ArgList @('--json', 'status') -DataDir $DirB
$revokedWrite = Invoke-Om -Name 'session-revoked-local-write-refused' -ArgList @('--json', 'remember', "must-not-write-$marker", '--title', 'must-not-write', '--force') -DataDir $DirB -AllowStatus @('fail')
Assert-Smoke 'session-revoked-device-readonly-enforced' ($revokedStatus.Ok -and [string]$revokedStatus.Envelope.summary.workspace -eq 'readonly' -and $revokedWrite.Ok -and $revokedWrite.Exit -eq 1 -and ($revokedWrite.Envelope.errors -join ' ') -match 'read.only')
$ownerClear = Invoke-Om -Name 'session-owner-clear-personal-readonly' -ArgList @('--json', 'agent-config', '--set', 'readonly=false') -DataDir $DirB
Assert-Smoke 'session-owner-personal-readonly-cleared' ($ownerClear.Ok -and [string]$ownerClear.Envelope.summary.key -eq 'readonly' -and $ownerClear.Envelope.summary.value -eq $false)
$ownerConfig = Invoke-Om -Name 'session-owner-agent-config-readback' -ArgList @('--json', 'agent-config') -DataDir $DirB
Assert-Smoke 'session-owner-personal-readonly-readback' ($ownerConfig.Ok -and $ownerConfig.Envelope.summary.readonly -eq $false -and $ownerConfig.Envelope.summary.readonly_team -ne $true)
$ownerStatus = Invoke-Om -Name 'session-owner-device-mode-readback' -ArgList @('--json', 'status') -DataDir $DirB
Assert-Smoke 'session-owner-device-normal-restored' ($ownerStatus.Ok -and [string]$ownerStatus.Envelope.summary.workspace -eq 'normal')
Invoke-Om -Name 'logout' -ArgList @('--json', 'logout') -DataDir $DirB | Out-Null
$loginAgain = Invoke-Om -Name 'login-b-again' -ArgList @('--json', 'login', '--interactive', '--addr', $Server, '--user', $user, '--pass', $pass, '--super', $super) -DataDir $DirB -Secret -TimeoutSec 180
if (-not $loginAgain.Ok) { throw 're-login failed' }
$recallAgain = Invoke-Om -Name 'recall-after-relogin' -ArgList @('--json', 'recall', $marker, '--limit', '3') -DataDir $DirB -TimeoutSec 180 -AllowIndexWait
if ($recallAgain.Stdout -notlike "*$marker*") { throw 'the marker was not found after re-login' }
$syncAgain = Invoke-Om -Name 'sync-after-relogin' -ArgList @('--json', 'sync') -DataDir $DirB -TimeoutSec 180
if (-not $syncAgain.Ok) { throw 'sync failed after re-login' }
$superReset = Invoke-Om -Name 'super-reset' -ArgList @('--json', 'super-reset', '--super', $super) -DataDir $DirA -Secret -TimeoutSec 180
$newSuper = [string]$superReset.Envelope.summary.super
if ($superReset.Ok -and $newSuper) { $mainCloudAccount.Super = $newSuper }
$afterReset = Invoke-Om -Name 'super-reset-memory-readback' -ArgList @('--json', 'show', $id) -DataDir $DirA
Assert-Smoke 'super-reset-preserves-memory' ($superReset.Ok -and [bool]$newSuper -and $newSuper -ne $super -and $afterReset.Ok -and [string]$afterReset.Envelope.details.entry.content -eq "merged $marker")
$oldRecoveryDir = Join-Path $Root 'old-recovery'
Invoke-Om -Name 'old-recovery-config' -ArgList @('--json', 'config', '--addr', $Server, '--autosync', 'false') -DataDir $oldRecoveryDir | Out-Null
$oldRecovery = Invoke-Om -Name 'super-reset-old-code-refused' -ArgList @('--json', 'login', '--interactive', '--addr', $Server, '--user', $user, '--pass', $pass, '--super', $super) -DataDir $oldRecoveryDir -Secret -HostProfile -AllowStatus @('fail')
Assert-Smoke 'super-reset-old-code-rejected' ($oldRecovery.Ok -and $oldRecovery.Exit -eq 1 `
    -and [string]$oldRecovery.Envelope.summary.reason -ceq 'account_outcome_unknown' `
    -and ($oldRecovery.Envelope.errors -join ' ').EndsWith('; login_kind=vault_unlock_rejected', [StringComparison]::Ordinal) `
    -and -not (Test-Path -LiteralPath (Join-Path $oldRecoveryDir 'session.json')) `
    -and (-not (Test-Path -LiteralPath (Join-Path $oldRecoveryDir 'accounts')) `
        -or @(Get-ChildItem -LiteralPath (Join-Path $oldRecoveryDir 'accounts') -Filter session.json -Recurse -ErrorAction Stop).Count -eq 0))
$recoveryDir = Join-Path $Root 'new-recovery'
Invoke-Om -Name 'new-recovery-config' -ArgList @('--json', 'config', '--addr', $Server, '--autosync', 'false') -DataDir $recoveryDir | Out-Null
$newRecovery = Invoke-Om -Name 'super-reset-new-code-login' -ArgList @('--json', 'login', '--interactive', '--addr', $Server, '--user', $user, '--pass', $pass, '--super', $newSuper) -DataDir $recoveryDir -Secret -HostProfile
$newRecoverySync = Invoke-Om -Name 'super-reset-new-code-sync' -ArgList @('--json', 'sync') -DataDir $recoveryDir
$newRecoveryRead = Invoke-Om -Name 'super-reset-new-code-readback' -ArgList @('--json', 'show', $id) -DataDir $recoveryDir
Assert-Smoke 'super-reset-new-code-recovers-cloud-content' ($newRecovery.Ok -and $newRecoverySync.Ok -and $newRecoveryRead.Ok -and [string]$newRecoveryRead.Envelope.details.entry.content -eq "merged $marker")
$recoverySession = Get-SmokeSession $recoveryDir $user
$fullLogout = Invoke-Om -Name 'cloud-full-logout' -ArgList @('--json', 'logout', '--full') -DataDir $recoveryDir -HostProfile
Assert-Smoke 'cloud-full-logout-removes-session' ($fullLogout.Ok -and [string]$fullLogout.Envelope.summary.mode -eq 'full' -and -not (Test-Path -LiteralPath $recoverySession.Path))
$recoveredAfterFull = Invoke-Om -Name 'cloud-login-after-full-logout' -ArgList @('--json', 'login', '--interactive', '--addr', $Server, '--user', $user, '--pass', $pass, '--super', $newSuper) -DataDir $recoveryDir -Secret -HostProfile
$fullRead = Invoke-Om -Name 'cloud-full-logout-login-readback' -ArgList @('--json', 'show', $id) -DataDir $recoveryDir
Assert-Smoke 'cloud-full-logout-recovery-preserves-library' ($recoveredAfterFull.Ok -and $fullRead.Ok -and [string]$fullRead.Envelope.details.entry.content -eq "merged $marker")

# A separate disposable library exercises mismatched key material without
# risking the main sweep account's local library or exporting old ciphertext.
$resetDir = Join-Path $Root 'reset-vault'
Invoke-Om -Name 'reset-vault-config' -ArgList @('--json', 'config', '--addr', $Server, '--autosync', 'false') -DataDir $resetDir | Out-Null
Invoke-Om -Name 'reset-vault-baseline-login' -ArgList @('--json', 'login', '--interactive', '--addr', $Server, '--user', $user, '--pass', $pass, '--super', $newSuper) -DataDir $resetDir -Secret -HostProfile | Out-Null
$resetKeys = Invoke-Om -Name 'reset-vault-local-keygen' -ArgList @('--json', 'keygen', '--force') -DataDir $resetDir -Secret -HostProfile
Complete-SmokeKeygen 'reset-vault-keygen' $resetKeys $resetDir
$mismatched = Invoke-Om -Name 'reset-vault-old-local-write' -ArgList @('--json', 'remember', "reset-local-old-key-$stamp", '--title', "reset-local-$stamp", '--importance', 'important', '--force') -DataDir $resetDir
$mismatchedId = [string]$mismatched.Envelope.summary.id
Assert-Smoke 'reset-vault-mismatched-fixture-written' ($resetKeys.Ok -and [bool]$resetKeys.Envelope.summary.super -and $mismatched.Ok -and [bool]$mismatchedId)
$resetProfile = [string](Invoke-Om -Name 'reset-vault-selected-profile' -ArgList @('--json', 'config') -DataDir $resetDir).Envelope.summary.data_dir
$resetSession = Join-Path $resetProfile 'session.json'
# Preserve the live binding until its owned process has acknowledged stop and exited.
$resetRuntime = Get-OwnedRuntimeHealth $resetDir
if ([IO.Path]::GetFullPath([string]$resetRuntime.data_dir) -ne [IO.Path]::GetFullPath($resetProfile)) {
    throw 'Reset fixture Runtime does not own the selected profile.'
}
$resetProcess = Get-Process -Id ([int]$resetRuntime.pid) -ErrorAction Stop
try {
    $null = $resetProcess.Handle
    $resetStopped = Invoke-Om -Name 'host-stop-before-profile-change' -ArgList @('--runtime-internal', '--stop') `
        -DataDir $resetDir -Raw -ExpectExit @(0) -NoReadyRetry
    if ($resetStopped.Ok -ne $true -or $resetStopped.Exit -ne 0 -or -not $resetProcess.WaitForExit(10000)) {
        throw 'Reset fixture owned Runtime stop and exit were not confirmed.'
    }
} finally { $resetProcess.Dispose() }
# keygen clears identity; bind this fixture to the same remote identity to exercise the overwrite guard.
$resetIdentity = Get-Content -LiteralPath $resetSession -Raw -Encoding utf8 | ConvertFrom-Json -AsHashtable
$resetIdentity['user'] = $user
$resetIdentity['addr'] = $Server
[IO.File]::WriteAllText($resetSession, ($resetIdentity | ConvertTo-Json -Depth 20) + "`n", [Text.UTF8Encoding]::new($false))
$beforeResetSession = (Get-FileHash -LiteralPath $resetSession -Algorithm SHA256).Hash
$resetJournalPath = Join-Path $resetDir '.login-recovery.json'
$beforeResetOperations = @((Get-Content -LiteralPath $resetJournalPath -Raw -Encoding utf8 | ConvertFrom-Json).operations |
    ForEach-Object { $_.operation_id })
$previousResetSuper = $env:RSRS_SUPER
try {
    # Keep the disposable local key usable without storing its Super password in the session.
    $env:RSRS_SUPER = [string]$resetKeys.Envelope.summary.super
    $resetGuard = Invoke-Om -Name 'reset-vault-mismatch-refused' -ArgList @('--direct', '--json', 'login', '--interactive', '--addr', $Server, '--user', $user, '--pass', $pass, '--super', $newSuper) -DataDir $resetDir -Secret -AllowStatus @('fail')
    $resetJournal = Get-Content -LiteralPath $resetJournalPath -Raw -Encoding utf8 | ConvertFrom-Json
    $resetOperations = @($resetJournal.operations | Where-Object { $_.operation_id -cnotin $beforeResetOperations })
    $resetRejection = if ($resetOperations.Count -eq 1) { $resetOperations[0] } else { $null }
    $resetComparison = if ($IsWindows) { [StringComparison]::OrdinalIgnoreCase } else { [StringComparison]::Ordinal }
    $resetRolledBack = $resetJournal.schema_version -is [long] -and $resetJournal.schema_version -eq 2 -and $null -eq $resetJournal.active `
        -and $resetOperations.Count -eq 1 -and $resetRejection.operation_id -is [string] `
        -and $resetRejection.operation_id -cmatch '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$' `
        -and $resetRejection.idempotency_key -is [string] -and $resetRejection.idempotency_key -ceq $resetRejection.operation_id `
        -and $resetRejection.phase -is [string] -and $resetRejection.phase -ceq 'rolled_back' `
        -and $resetRejection.account.register -is [bool] -and $resetRejection.account.register -ceq $false `
        -and $resetRejection.account.user -is [string] -and $resetRejection.account.user -ceq $user `
        -and $resetRejection.account.addr -is [string] -and $resetRejection.account.addr -ceq $Server `
        -and $resetRejection.original_sha256 -is [string] -and $resetRejection.original_sha256 -ceq $beforeResetSession.ToLowerInvariant() `
        -and $resetRejection.root -is [string] -and $resetRejection.profile -is [string] `
        -and $null -eq $resetRejection.candidate -and $null -eq $resetRejection.backend -and $null -eq $resetRejection.account.target `
        -and ([IO.Path]::GetFullPath(([string]$resetRejection.root -replace '^\\\\\?\\', ''))).Equals([IO.Path]::GetFullPath($resetDir), $resetComparison) `
        -and ([IO.Path]::GetFullPath(([string]$resetRejection.profile -replace '^\\\\\?\\', ''))).Equals([IO.Path]::GetFullPath(($resetProfile -replace '^\\\\\?\\', '')), $resetComparison)
    $resetExpectedError = if ($resetRolledBack) {
        "account operation rolled back; operation_id=$($resetRejection.operation_id) source_profile=$($resetRejection.profile); original account was preserved"
    } else { '' }
    Assert-Smoke 'reset-vault-mismatch-guard' ($resetGuard.Ok -and $resetGuard.Exit -eq 1 `
        -and [string]$resetGuard.Envelope.command -ceq 'cli' -and [string]$resetGuard.Envelope.status -ceq 'fail' `
        -and [string]$resetGuard.Envelope.summary.reason -ceq 'runtime_error' `
        -and [string]$resetGuard.Envelope.details.error_type -ceq 'runtime' -and $resetRolledBack `
        -and @($resetGuard.Envelope.errors).Count -eq 1 `
        -and $resetGuard.Envelope.errors[0] -is [string] -and $resetGuard.Envelope.errors[0] -ceq $resetExpectedError)
    Assert-Smoke 'login-key-mismatch-original-session-preserved' ((Get-FileHash -LiteralPath $resetSession -Algorithm SHA256).Hash -ceq $beforeResetSession)
    $resetRead = Invoke-Om -Name 'login-key-mismatch-original-readback' -ArgList @('--json', 'show', $mismatchedId) -DataDir $resetDir
    Assert-Smoke 'login-key-mismatch-original-data-readable' ($resetRead.Ok -and [string]$resetRead.Envelope.details.entry.content -ceq "reset-local-old-key-$stamp")
} finally {
    if ($null -eq $previousResetSuper) { Remove-Item Env:RSRS_SUPER -ErrorAction SilentlyContinue }
    else { $env:RSRS_SUPER = $previousResetSuper }
}
Invoke-Om -Name 'sync-reset' -ArgList @('--json', 'sync-reset') -DataDir $DirA -TimeoutSec 180 -Note 'snapshot rebuild on a disposable account' | Out-Null

# Web opens the hosted dashboard; runtime lifecycle is a separate hidden command.
$dashboardDir = Join-Path $Root 'dashboard-launch'
$dashboard = Invoke-Om -Name 'web-dashboard-report' -ArgList @('--json', 'web', '--no-open') -DataDir $dashboardDir
Assert-Smoke 'web-dashboard-fixed-url' ($dashboard.Ok -and [string]$dashboard.Envelope.summary.url -ceq 'https://dash.rsrs.rs' -and $dashboard.Envelope.summary.opened -eq $false)
Assert-Smoke 'web-dashboard-no-runtime' (-not (Test-Path -LiteralPath (Join-Path $dashboardDir 'runtime/endpoint.json')))
$legacyWebRejected = $true
foreach ($flags in @(@('--internal'), @('--status'), @('--stop'), @('--port', '15169'), @('--host', '127.0.0.1'))) {
    $rejected = Invoke-Om -Name "web-reject-legacy-$($flags[0].TrimStart('-'))" -ArgList (@('web') + $flags) -DataDir $dashboardDir -ExpectExit @(1, 2) -Raw
    $legacyWebRejected = $legacyWebRejected -and $rejected.Ok
}
Assert-Smoke 'web-legacy-lifecycle-flags-rejected' ($legacyWebRejected -and -not (Test-Path -LiteralPath (Join-Path $dashboardDir 'runtime/endpoint.json')))
# A normal CLI call starts only the API runtime for the original fixture.
Invoke-Om -Name 'runtime-start-by-cli-status' -ArgList @('--json', 'status') -DataDir $DirA | Out-Null
$webStatus = Invoke-Om -Name 'runtime-status' -ArgList @('--json', '--runtime-internal', '--status') -DataDir $DirA -ExpectExit @(0, 2) -Raw
$url = ''
$endpointFile = Join-Path $DirA 'runtime\endpoint.json'
if (Test-Path $endpointFile) {
    $ep = Get-Content $endpointFile -Raw | ConvertFrom-Json
    $url = [string]$ep.url
    $runtimeUri = [Uri]$url
    if ($runtimeUri.Host -notin @('127.0.0.1', 'localhost', '::1')) { throw "Runtime endpoint is not loopback: $url" }
}
$tokenFile = Join-Path $DirA 'runtime\token'
$origin = $url
if ($origin -match '^(https?://[^/?]+)') { $origin = $Matches[1] }
$mcpUrl = ''
if ($origin) { $mcpUrl = ($origin.TrimEnd('/') + '/mcp') }
$mcpSse = ''
if ($origin) { $mcpSse = ($origin.TrimEnd('/') + '/sse') }
$mcpHeaders = @{}
Assert-Smoke 'runtime-endpoint-without-token' ([bool]$origin -and -not (Test-Path -LiteralPath $tokenFile))
function Invoke-LocalSmoke {
    param([string]$Name, [string]$Path, $Payload, [int]$ExpectedStatus = 200, [hashtable]$Headers = $mcpHeaders, [string]$Method = 'POST', [switch]$RawBody)
    $body = if ($RawBody) { [string]$Payload } else { $Payload | ConvertTo-Json -Compress -Depth 12 }
    $request = @{ Uri = "$origin$Path"; Method = $Method; Headers = $Headers; TimeoutSec = 90; SkipHttpErrorCheck = $true }
    if ($Method -ne 'GET') { $request.Body = $body; $request.ContentType = 'application/json; charset=utf-8' }
    $response = Invoke-WebRequest @request
    Assert-Smoke $Name ([int]$response.StatusCode -eq $ExpectedStatus)
    if ($ExpectedStatus -eq 200) { return ($response.Content | ConvertFrom-Json) }
}
# The endpoint is the separate Runtime, whose installation has its own version and digest.
$health = Get-OwnedRuntimeHealth $DirA -TimeoutSec 90
Assert-Smoke 'runtime-http-health' ($null -ne $health -and $health.store_ready -eq $true)
Assert-Smoke 'runtime-health-protocol-version' ([int]$health.v -eq 1 -and [string]$health.bin -ceq [string]$health.runtime_version)
$wrongToken = Invoke-OwnedRuntimeRpc $DirA 'runtime.status' -RequestId 'wrong-token' `
    -Headers @{ Authorization = 'Bearer invalid-smoke-token' } -TimeoutSec 90 -FullResponse
Assert-Smoke 'runtime-http-loopback-ignores-token' ($wrongToken.ok -eq $true -and [string]$wrongToken.id -ceq 'wrong-token')
Invoke-LocalSmoke -Name 'runtime-http-foreign-origin' -Path '/api/rpc' -Payload @{ v = 1; id = 'foreign-origin'; method = 'runtime.status' } -Headers @{ Origin = 'https://example.invalid' } -ExpectedStatus 403 | Out-Null
Invoke-LocalSmoke -Name 'runtime-http-malformed-json' -Path '/api/rpc' -Payload '{' -RawBody -ExpectedStatus 400 | Out-Null
Invoke-LocalSmoke -Name 'runtime-http-missing-command' -Path '/api/rpc' -Payload @{} -ExpectedStatus 400 | Out-Null
foreach ($removed in @(@('/', 'GET'), @('/index.html', 'GET'), @('/favicon.ico', 'GET'), @('/api/invoke', 'POST'), @('/api/task/start', 'POST'))) {
    Invoke-LocalSmoke -Name "runtime-removed-ui-$($removed[0] -replace '[^a-zA-Z0-9]', '-')" -Path $removed[0] -Method $removed[1] -Payload @{} -ExpectedStatus 404 | Out-Null
}
function Invoke-RuntimeCli([string]$Name, [string[]]$Arguments, [int]$ExpectedExit = 0) {
    $response = Invoke-OwnedRuntimeRpc $DirA 'cli.exec' (@('--json') + $Arguments) `
        -RequestId $Name -ExpectedExit $ExpectedExit -TimeoutSec 90 -FullResponse
    Assert-Smoke $Name ($response.exit -eq $ExpectedExit)
    Assert-Smoke "$Name-response" ($response.ok -eq ($ExpectedExit -eq 0) -and [string]$response.id -ceq $Name `
        -and $null -ne $response.envelope -and ($ExpectedExit -eq 0 -or [string]$response.envelope.status -ceq 'fail'))
    return $response.envelope
}
