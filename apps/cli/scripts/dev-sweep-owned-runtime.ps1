function Invoke-OwnedRuntimeRpc {
    param([string]$DataDir, [string]$Method, [string[]]$Arguments = @(), [int]$TimeoutSec = 5,
        [string]$RequestId = '', [hashtable]$Headers = @{}, [int]$ExpectedExit = 0, [switch]$FullResponse)
    $endpoint = Get-Content -LiteralPath (Join-Path $DataDir 'runtime/endpoint.json') -Raw | ConvertFrom-Json
    $uri = [Uri][string]$endpoint.url
    if ($uri.Scheme -ne 'http' -or $uri.Host -notin @('127.0.0.1', 'localhost', '::1')) {
        throw 'Owned runtime endpoint is not loopback HTTP.'
    }
    if (Test-Path -LiteralPath (Join-Path $DataDir 'runtime/token')) { throw 'Owned loopback runtime created a token.' }
    $origin = $uri.GetLeftPart([UriPartial]::Authority)
    try {
        if ($Method -eq 'health') {
            return Invoke-RestMethod -Uri "$origin/api/health" -TimeoutSec $TimeoutSec
        }
        $health = Get-OwnedRuntimeHealth $DataDir -TimeoutSec $TimeoutSec
        $scope = @{ account_id = $health.account_id; instance_id = $health.instance_id; generation = $health.generation }
        if (-not $RequestId) { $RequestId = [Guid]::NewGuid().ToString('N') }
        $body = @{ v = 1; id = $RequestId; scope = $scope; method = $Method; args = $Arguments } | ConvertTo-Json -Compress -Depth 8
        $response = Invoke-RestMethod -Uri "$origin/api/rpc" -Method Post -ContentType 'application/json' `
            -Body $body -Headers $Headers -TimeoutSec $TimeoutSec
        if ($response.ok -isnot [bool] -or $response.ok -ne ($ExpectedExit -eq 0) -or $response.exit -cne $ExpectedExit `
            -or $response.v -ne 1 -or $response.pid -cne $health.pid -or $null -eq $response.envelope `
            -or [string]$response.id -cne $RequestId -or [string]$response.bin -cne [string]$health.bin `
            -or [string]$response.scope.account_id -cne [string]$scope.account_id `
            -or [string]$response.scope.instance_id -cne [string]$scope.instance_id `
            -or $response.scope.generation -cne $scope.generation) {
            throw 'Owned runtime RPC response identity failed.'
        }
        if ($FullResponse) { return $response }
        return $response.envelope.summary
    } catch { throw 'Owned runtime request failed.' }
}

function Get-OwnedRuntimeHealth([string]$DataDir, [int]$TimeoutSec = 5) {
    $health = Invoke-OwnedRuntimeRpc $DataDir 'health' -TimeoutSec $TimeoutSec
    $profile = [IO.Path]::GetFullPath([string]$health.data_dir)
    $device = [IO.Path]::GetFullPath($DataDir).TrimEnd([IO.Path]::DirectorySeparatorChar)
    $nativeName = if ($IsWindows) { 'rsrs.exe' } else { 'rsrs' }
    $runtimeVersion = $ExpectVersion
    $runtimeHash = $script:BinarySha256
    $ownedExecutable = [IO.Path]::GetFullPath((Join-Path $DataDir "bin/$nativeName"))
    if ($script:PublishedRuntimePaths.ContainsKey($DataDir)) {
        $ownedExecutable = [IO.Path]::GetFullPath($script:PublishedRuntimePaths[$DataDir])
        $runtimeVersion = if ($env:RSRS_ARTIFACT_SOURCE -ceq 'published-assets') { $env:RSRS_PUBLISHED_RUNTIME_VERSION } else { $env:NATIVE_RUNTIME_VERSION }
        $runtimeHash = if ($env:RSRS_ARTIFACT_SOURCE -ceq 'published-assets') { $env:RSRS_PUBLISHED_RUNTIME_SHA256 } else { $env:NATIVE_RUNTIME_SHA256 }
        if ([string]$health.runtime_version -cne $runtimeVersion) { throw 'Owned runtime version differs from the selected installation.' }
    }
    $reportedExecutable = [IO.Path]::GetFullPath([string]$health.exe)
    if ($IsWindows) {
        $profile = $profile -replace '^\\\\\?\\', ''
        $device = $device -replace '^\\\\\?\\', ''
        $ownedExecutable = $ownedExecutable -replace '^\\\\\?\\', ''
        $reportedExecutable = $reportedExecutable -replace '^\\\\\?\\', ''
    }
    if ([int]$health.v -ne 1 -or [string]$health.bin -cne $runtimeVersion -or [int]$health.pid -le 0 `
        -or ($profile -ne $device -and -not $profile.StartsWith($device + [IO.Path]::DirectorySeparatorChar)) `
        -or $reportedExecutable -ne $ownedExecutable `
        -or (Get-FileHash -LiteralPath $ownedExecutable -Algorithm SHA256).Hash.ToLowerInvariant() -cne $runtimeHash `
        -or [string]::IsNullOrWhiteSpace([string]$health.account_id) `
        -or [string]::IsNullOrWhiteSpace([string]$health.instance_id) `
        -or ($health.generation -isnot [long] -and $health.generation -isnot [int]) -or $health.generation -le 0) {
        throw 'Owned runtime health identity failed.'
    }
    return $health
}

function Test-RuntimeChanged($Before, $After, [string]$Profile) {
    # Shutdown and process reaping are asynchronous; bound the observation only.
    for ($i = 0; $i -lt 20 -and (Get-Process -Id ([int]$Before.pid) -ErrorAction SilentlyContinue); $i++) {
        Start-Sleep -Milliseconds 50
    }
    return [int]$Before.pid -ne [int]$After.pid `
        -and -not (Get-Process -Id ([int]$Before.pid) -ErrorAction SilentlyContinue) `
        -and [IO.Path]::GetFullPath([string]$After.data_dir) -eq [IO.Path]::GetFullPath($Profile)
}

function Test-RuntimeUnchanged($Before, $After, [string]$Profile) {
    # Invalid names fail before the coordinator: accepting a replacement actor hides a regression.
    return [int]$Before.pid -gt 0 -and [int]$Before.pid -eq [int]$After.pid `
        -and $null -ne (Get-Process -Id ([int]$Before.pid) -ErrorAction SilentlyContinue) `
        -and [string]$Before.data_dir -ceq [string]$After.data_dir `
        -and [IO.Path]::GetFullPath([string]$After.data_dir) -eq [IO.Path]::GetFullPath($Profile) `
        -and -not [string]::IsNullOrWhiteSpace([string]$Before.account_id) `
        -and [string]$Before.account_id -ceq [string]$After.account_id `
        -and -not [string]::IsNullOrWhiteSpace([string]$Before.instance_id) `
        -and [string]$Before.instance_id -ceq [string]$After.instance_id `
        -and $Before.generation -gt 0 -and $Before.generation -ceq $After.generation `
        -and [string]$Before.exe -ceq [string]$After.exe `
        -and [string]$Before.bin -ceq [string]$After.bin `
        -and [string]$Before.runtime_version -ceq [string]$After.runtime_version `
        -and $Before.store_ready -eq $true -and $After.store_ready -eq $true
}

function Get-AccountRejectionSnapshot([string]$Device, [string]$Profile) {
    # Snapshot only stable control/session state; the serving actor may update its own DB/WAL.
    $paths = @((Join-Path $Device 'client.json'), (Join-Path $Device 'runtime/endpoint.json'), (Join-Path $Profile 'session.json'))
    $files = @($paths | ForEach-Object { [Convert]::ToBase64String([IO.File]::ReadAllBytes($_)) })
    $profiles = @(Get-ChildItem -LiteralPath (Join-Path $Device 'accounts') -Force | Sort-Object Name | ForEach-Object { $_.Name })
    return ([ordered]@{ files = $files; profiles = $profiles; invalid_exists = (Test-Path -LiteralPath (Join-Path $Device 'invalid')) } | ConvertTo-Json -Depth 5 -Compress)
}

function Wait-OwnedModelIdle([string]$DataDir, [int]$TimeoutSec) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $taskId = [Guid]::NewGuid().ToString('N')
    while ($clock.Elapsed.TotalSeconds -lt [Math]::Min(120, $TimeoutSec)) {
        $health = Get-OwnedRuntimeHealth $DataDir
        $state = [string]$health.retrieval_index.state
        if ($state -ceq 'failed') { throw 'Owned retrieval index failed before model installation.' }
        if ($state -cnotin @('idle', 'scheduled', 'running', 'ready')) { throw 'Owned retrieval index readiness state is invalid.' }
        $progress = Invoke-OwnedRuntimeRpc $DataDir 'model.control' @($taskId, 'false')
        if ($progress.active -ne $true -and $progress.active -ne $false) { throw 'Model progress active flag is missing.' }
        if ($state -ceq 'ready' -and $progress.active -eq $false) {
            return [int][Math]::Floor($TimeoutSec - $clock.Elapsed.TotalSeconds)
        }
        Start-Sleep -Milliseconds 100
    }
    throw 'Owned model/index work did not finish within the bounded installation deadline.'
}

# Host keygen commits keys but deliberately leaves its original operation Paused.
# Only a positively completed keygen may request the normal explicit Start once.
function Complete-SmokeKeygen([string]$Name, $Keygen, [string]$DataDir) {
    Assert-Smoke "$Name-committed" ($Keygen.Ok -eq $true -and $Keygen.Exit -eq 0 `
        -and [string]$Keygen.Envelope.command -ceq 'keygen' `
        -and [string]$Keygen.Envelope.status -ceq 'ok' `
        -and [bool]$Keygen.Envelope.summary.super `
        -and -not [string]::IsNullOrWhiteSpace([string]$Keygen.Envelope.summary.path) `
        -and @($Keygen.Envelope.actions) -ccontains 'runtime start')
    $profile = [IO.Path]::GetFullPath((Split-Path -Parent ([string]$Keygen.Envelope.summary.path)))
    $device = [IO.Path]::GetFullPath($DataDir).TrimEnd([IO.Path]::DirectorySeparatorChar)
    if ($IsWindows) {
        $profile = $profile -replace '^\\\\\?\\', ''
        $device = $device -replace '^\\\\\?\\', ''
    }
    Assert-Smoke "$Name-owned-profile" ($profile -eq $device -or $profile.StartsWith($device + [IO.Path]::DirectorySeparatorChar))
    # No HostProfile stop/direct wrapper and no readiness retry: an uncertain
    # Start must retain its durable evidence, not replay or replace the operation.
    $started = Invoke-Om -Name "$Name-explicit-start" -ArgList @('--json', 'runtime', 'start') `
        -DataDir $DataDir -AllowStatus @('ok') -NoReadyRetry
    Assert-Smoke "$Name-started" ($started.Ok -eq $true -and $started.Exit -eq 0 `
        -and [string]$started.Envelope.command -ceq 'runtime.start' `
        -and [string]$started.Envelope.status -ceq 'ok')
    $health = Get-OwnedRuntimeHealth $DataDir
    $activeProfile = [IO.Path]::GetFullPath([string]$health.data_dir)
    if ($IsWindows) {
        $profile = $profile -replace '^\\\\\?\\', ''
        $activeProfile = $activeProfile -replace '^\\\\\?\\', ''
    }
    Assert-Smoke "$Name-ready" ($activeProfile -eq $profile -and $health.store_ready -eq $true)
}

function Invoke-Om {
    param(
        [Parameter(Mandatory)][string]$Name,
        [string[]]$ArgList = @(),
        [Parameter(Mandatory)][string]$DataDir,
        [int[]]$ExpectExit = @(0),
        [string[]]$AllowStatus = @('ok', 'warn', 'skip'),
        [switch]$Secret,
        [switch]$Raw,
        [switch]$HostProfile,
        [switch]$NoReadyRetry,
        [switch]$AllowDestructiveMcpFixture,
        [switch]$AllowIndexWait,
        [string]$ModelTaskId = '',
        [int]$TimeoutSec = 180,
        [string]$StdinText = '',
        [string]$Note = '',
        [int]$MinStdout = 0,
        [string]$RequireCommand = ''
    )
    # These are separate simulated devices sharing one CI host/port. The host
    # must stop the old device with its own credentials before switching roots.
    if ($script:ActiveDataDir -and $script:ActiveDataDir -ne $DataDir) {
        Invoke-Om -Name 'host-stop-before-device-switch' -ArgList @('--runtime-internal', '--stop') `
            -DataDir $script:ActiveDataDir -Raw | Out-Null
    }
    $script:ActiveDataDir = $DataDir
    if ($HostProfile) {
        # Only stop this disposable sweep runtime. Ordinary profile changes may
        # autostart a replacement; successful host keygen instead requires explicit Start.
        Invoke-Om -Name 'host-stop-before-profile-change' -ArgList @('--runtime-internal', '--stop') `
            -DataDir $DataDir -Raw -ExpectExit @(0, 2) | Out-Null
        Stop-OurRuntimes
        if ($ArgList -notcontains '--direct') { $ArgList = @('--direct') + $ArgList }
    }
    if ($ModelTaskId) { $ArgList = @('--model-task-id', $ModelTaskId) + $ArgList }
    if ($AllowIndexWait) {
        $readCommand = @($ArgList | Where-Object { $_ -notin @('--direct', '--client-only', '--json') })[0]
        if ($readCommand -notin @('recall', 'candidates') -or $Raw -or $Secret -or $ModelTaskId) {
            throw 'Index readiness waiting is restricted to explicit JSON retrieval reads.'
        }
    }
    New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
    $script:Step++
    $safe = ($Name -replace '[^\w\-]+', '_')
    $outFile = Join-Path $Root "logs\$($script:Step)-$safe.out.txt"
    $errFile = Join-Path $Root "logs\$($script:Step)-$safe.err.txt"
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    if ($script:UseExe) {
        $psi.FileName = $script:ExePath
    } else {
        $psi.FileName = 'node'
        $psi.ArgumentList.Add($script:CliJs)
    }
    $psi.WorkingDirectory = $Work
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.RedirectStandardInput = $true
    $utf8 = [System.Text.UTF8Encoding]::new($false)
    $psi.StandardOutputEncoding = $utf8
    $psi.StandardErrorEncoding = $utf8
    $psi.StandardInputEncoding = $utf8
    foreach ($a in $ArgList) { $psi.ArgumentList.Add([string]$a) }
    foreach ($tokenName in @('GH_TOKEN', 'GITHUB_TOKEN')) {
        [void]$psi.Environment.Remove($tokenName)
    }
    $psi.Environment['RSRS_DATA_DIR'] = $DataDir
    # Host unlock needs authentication; keep each disposable device's token in memory.
    if (-not $script:RpcTokens.ContainsKey($DataDir)) {
        $script:RpcTokens[$DataDir] = [Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
    }
    $psi.Environment['RSRS_RPC_TOKEN'] = $script:RpcTokens[$DataDir]
    $psi.Environment['RSRS_LANG'] = 'en'
    $psi.Environment['HOME'] = $SmokeHome
    $psi.Environment['USERPROFILE'] = $SmokeHome
    $psi.Environment['XDG_CONFIG_HOME'] = Join-Path $SmokeHome '.config'
    $psi.Environment['XDG_DATA_HOME'] = Join-Path $SmokeHome '.local/share'
    $psi.Environment['RSRS_M3_DIR'] = Join-Path $Root 'models/bge-m3'
    $psi.Environment['RSRS_M3_DIR'] = Join-Path $Root 'models/bge-m3'
    if ($env:RSRS_ARTIFACT_SOURCE -ceq 'published-assets' -or
        ($env:ARTIFACT_SOURCE -ceq 'build' -and $env:VALIDATION_PROFILE -ceq 'full' -and
         $env:GITHUB_ACTIONS -ceq 'true' -and $env:RUNNER_ENVIRONMENT -ceq 'github-hosted')) {
        if (-not $script:PublishedRuntimePaths.ContainsKey($DataDir) -or $env:RSRS_ARTIFACT_SOURCE -cne 'published-assets') {
            $install = [System.Diagnostics.ProcessStartInfo]::new()
            $install.FileName = 'python'
            $install.UseShellExecute = $false
            $install.CreateNoWindow = $true
            $install.RedirectStandardOutput = $true
            $install.RedirectStandardError = $true
            $install.Environment.Clear()
            foreach ($pair in $psi.Environment.GetEnumerator()) { $install.Environment[$pair.Key] = $pair.Value }
            $install.ArgumentList.Add((Join-Path $PSScriptRoot 'native_fixture_process.py'))
            $install.ArgumentList.Add('--install-published')
            $install.ArgumentList.Add($script:ExePath)
            $child = [System.Diagnostics.Process]::Start($install)
            try {
                if (-not $child.WaitForExit(210000)) {
                    $child.Kill($true)
                    $child.WaitForExit()
                    throw 'Selected Runtime offline installation timed out.'
                }
                if ($child.ExitCode -ne 0) { throw 'Selected Runtime offline installation failed.' }
                $script:PublishedRuntimePaths[$DataDir] = $child.StandardOutput.ReadToEnd().Trim()
            } finally { $child.Dispose() }
        }
        $psi.Environment['RSRS_RUNTIME_BIN'] = $script:PublishedRuntimePaths[$DataDir]
    }
    # Only this disposable child may opt into the three named lifecycle tools.
    # Parent environment and all normal/default-policy calls remain unchanged.
    foreach ($policyPrefix in @('RSRS_', 'ONEMEMORY_', 'RESPIRE_')) {
        foreach ($policyName in @('MCP_ALLOW','MCP_DENY','MCP_ALLOW_TOOLS','MCP_DENY_TOOLS')) {
            [void]$psi.Environment.Remove($policyPrefix + $policyName)
        }
    }
    if ($AllowDestructiveMcpFixture) {
        $psi.Environment['RSRS_MCP_ALLOW'] = 'memory_show,memory_forget,memory_restore'
    }
    [void]$psi.Environment.Remove('RSRS_SERVER')
    Write-Host "STEP $script:Step $Name"
    $readyTries = 0
    $indexRetried = $false
    $indexEvent = $null
    $indexCommandBudget = [Diagnostics.Stopwatch]::StartNew()
    do {
        $readyTries++
        if ($readyTries -gt 1) { Write-Host "RETRY $Name" }
    $attemptTimeout = $TimeoutSec
    if ($AllowIndexWait) {
        $attemptTimeout = [int][Math]::Floor($TimeoutSec - $indexCommandBudget.Elapsed.TotalSeconds)
        if ($attemptTimeout -le 0) { throw 'Index readiness retry exceeded the original command deadline.' }
    }
    $proc = [System.Diagnostics.Process]::Start($psi)
    if ($StdinText) { $proc.StandardInput.Write($StdinText) }
    $proc.StandardInput.Close()
    $outTask = $proc.StandardOutput.ReadToEndAsync()
    $errTask = $proc.StandardError.ReadToEndAsync()
    $timedOut = $false
    $progressSamples = New-Object System.Collections.Generic.List[object]
    $finished = $false
    if ($ModelTaskId) {
        $waitStarted = [Diagnostics.Stopwatch]::StartNew()
        while (-not ($finished = $proc.WaitForExit(25)) -and $waitStarted.Elapsed.TotalSeconds -lt $TimeoutSec) {
            $progress = Invoke-OwnedRuntimeRpc $DataDir 'model.control' @($ModelTaskId, 'false')
            if ($progress.active -eq $true) {
                $progressSamples.Add($progress) | Out-Null
            } elseif ($progress.active -ne $false) { throw 'Model progress active flag is missing.' }
        }
    } else { $finished = $proc.WaitForExit($attemptTimeout * 1000) }
    if (-not $finished) {
        $timedOut = $true
        try { $proc.Kill($true) } catch { try { $proc.Kill() } catch { } }
        Stop-OurRuntimes
        $proc.WaitForExit(5000) | Out-Null
    }
    $stdout = ''
    $stderr = ''
    if ($outTask.Wait(10000)) { try { $stdout = $outTask.Result } catch { $stdout = '' } }
    if ($errTask.Wait(10000)) { try { $stderr = $errTask.Result } catch { $stderr = '' } }
    $exit = if ($timedOut) { 124 } else { $proc.ExitCode }
    $blob = $stdout + "`n" + $stderr
    if ($ModelTaskId) {
        $finalProgress = Invoke-OwnedRuntimeRpc $DataDir 'model.control' @($ModelTaskId, 'false')
        $script:ModelProgressEvidence = @{
            observed = $progressSamples.Count -gt 0; samples = @($progressSamples | ForEach-Object {
                @{ phase = $_.phase; done = $_.done; total = $_.total; elapsed = $_.elapsed; idle = $_.idle }
            })
            completed_inactive = $finalProgress.active -eq $false
            limitation = 'A completed operation clears progress; a fast operation may finish before a poll observes it. The 15-second foreground budget branch is not exercised by this fixture.'
        }
    }
    if ($Secret) {
        [System.IO.File]::WriteAllText((Join-Path $Secrets "$script:Step-$safe.txt"), $blob, [Text.UTF8Encoding]::new($false))
        $stdoutForScan = $stdout
    } else {
        [System.IO.File]::WriteAllText($outFile, $stdout, [Text.UTF8Encoding]::new($false))
        [System.IO.File]::WriteAllText($errFile, $stderr, [Text.UTF8Encoding]::new($false))
        $stdoutForScan = $stdout
    }
    $leaks = @(Get-Leak $stdoutForScan $Secret.IsPresent)
    $envlp = Get-Envelope $stdout
    Assert-DevEnvelope $envlp $Name
    $status = ''
    if ($envlp -and $envlp.PSObject.Properties['status']) { $status = [string]$envlp.status }
    if ($envlp -and [string]$envlp.command -eq 'cli') {
        $keys = @($envlp.summary.PSObject.Properties.Name)
        if ($keys.Count -eq 0) { $Note = ("RPC returned an empty cli envelope " + $Note).Trim() }
    }
    if ($Raw -or -not $status) {
        $exitOk = $ExpectExit -contains [int]$exit
        $statusOk = $true
    } else {
        $want = switch ($status) { 'ok' { 0 } 'skip' { 0 } 'warn' { 2 } 'pending' { 2 } 'fail' {
            if ([string]$envlp.summary.reason -eq 'invalid_input' -and [string]$envlp.details.error_type -eq 'user') { 2 } else { 1 }
        } default { -1 } }
        $exitOk = ($exit -eq $want)
        $statusOk = $AllowStatus -contains $status
    }
    if ($stdout.Length -lt $MinStdout) { $statusOk = $false; $Note = ("stdout too short " + $Note).Trim() }
    if ($RequireCommand) {
        $got = [string]$envlp.command
        $total = $null
        if ($envlp -and $envlp.summary) { $total = $envlp.summary.total }
        if ($got -ne $RequireCommand -or $null -eq $total) {
            $statusOk = $false
            $Note = ("command=$got total=$total want $RequireCommand " + $Note).Trim()
        }
    }
    $readyFail = -not $NoReadyRetry -and (($stdout + $stderr) -match 'did not become ready')
        if ($readyFail -and $readyTries -lt 2) { Stop-OurRuntimes; Start-Sleep -Seconds 1 }
    $retryIndexPending = $AllowIndexWait -and -not $indexRetried -and -not $timedOut `
        -and $exit -eq 2 -and $status -ceq 'pending' -and [string]$envlp.summary.state -ceq 'index_pending' `
        -and @($envlp.errors).Count -eq 0 -and $leaks.Count -eq 0
    if ($retryIndexPending) {
        $indexRetried = $true
        # Preserve the first response separately. It is an observed Pending,
        # not a passing command or a replacement for the final data assertions.
        [IO.File]::WriteAllText($outFile + '.index-pending', $stdout, [Text.UTF8Encoding]::new($false))
        [IO.File]::WriteAllText($errFile + '.index-pending', $stderr, [Text.UTF8Encoding]::new($false))
        $indexEvent = [ordered]@{ case = $Name; exit = 2; state = 'index_pending'; readiness = 'waiting'; polls = 0; wait_ms = 0; retried = $false }
        $script:IndexPendingEvents.Add($indexEvent) | Out-Null
        Write-SweepReport
        $indexWait = [Diagnostics.Stopwatch]::StartNew()
        $indexWaitBudget = [Math]::Min(120, $TimeoutSec - $indexCommandBudget.Elapsed.TotalSeconds)
        while ($true) {
            $remaining = [Math]::Min($indexWaitBudget - $indexWait.Elapsed.TotalSeconds, $TimeoutSec - $indexCommandBudget.Elapsed.TotalSeconds)
            if ($remaining -lt 1) {
                $indexEvent.readiness = 'timeout'
                throw 'Owned retrieval index did not become ready within the bounded command deadline.'
            }
            $health = Get-OwnedRuntimeHealth $DataDir -TimeoutSec ([int][Math]::Min(5, [Math]::Floor($remaining)))
            $indexEvent.polls++
            $indexEvent.wait_ms = [int]$indexWait.Elapsed.TotalMilliseconds
            $indexState = [string]$health.retrieval_index.state
            if ($indexState -ceq 'failed') {
                $indexEvent.readiness = 'failed'
                throw 'Owned retrieval index reported failed; the command was not retried.'
            }
            if ($indexState -ceq 'ready') {
                $indexEvent.readiness = 'ready'
                $indexEvent.retried = $true
                break
            }
            if ($indexState -cnotin @('idle', 'scheduled', 'running')) { throw 'Owned retrieval index readiness state is invalid.' }
            Start-Sleep -Milliseconds 100
        }
    }
    } while (($readyFail -and $readyTries -lt 2) -or $retryIndexPending)
    if ($readyTries -gt 1) { $Note = ("runtime retried $readyTries time(s) " + $Note).Trim() }
    $ok = $exitOk -and $statusOk -and ($leaks.Count -eq 0) -and (-not $timedOut)
    $commandTokens = @($ArgList | Where-Object { $_ -notin @('--direct', '--client-only', '--json') })
    $commandPath = ''
    foreach ($entry in @($script:Catalog.command_paths | Sort-Object { $_.command.Length } -Descending)) {
        $pathTokens = @($entry.command -split ' ')
        if ($commandTokens.Count -ge $pathTokens.Count -and ($commandTokens[0..($pathTokens.Count - 1)] -join ' ') -eq $entry.command) {
            $commandPath = $entry.command
            break
        }
    }
    $registerState = @{ account_register_vault_pending = $null; account_register_vault_published = $null }
    if (-not $ok) {
        $failureDiagnostic = ("failure_reason=$(Get-SafeFailureReason $envlp) $(Get-SafeAccountPhase $envlp $DataDir $resolvedRoot $registerState)").Trim()
        $Note = ("$failureDiagnostic $Note").Trim()
    }
    $row = [pscustomobject]@{
        Step   = $script:Step
        Name   = $Name
        Command = $commandPath
        account_register_vault_pending = $registerState.account_register_vault_pending
        account_register_vault_published = $registerState.account_register_vault_published
        Exit   = $exit
        Status = $status
        Ok     = $ok
        Leak   = ($leaks -join ',')
        Note   = $(if ($timedOut) { "timeout ${TimeoutSec}s; $Note" } else { $Note })
    }
    $Rows.Add($row) | Out-Null
    Write-SweepReport -Force:(-not $ok)
    $flag = if (-not $ok) { 'FAIL' } elseif ($status -eq 'fail') { 'EXPECTED_ERROR' } elseif ($status -eq 'skip') { 'SKIP' } else { 'PASS' }
    Write-Host "$flag $Name exit=$exit status=$status leak=$($row.Leak)"
    if (-not $ok) {
        $detail = if ($Secret -or ($envlp.summary.reason -is [string] -and $envlp.summary.reason -ceq 'account_outcome_unknown')) { $failureDiagnostic } else { $stderr + ' ' + $stdout }
        $detail = [regex]::Replace($detail, '(?i)(super|secret|token|password|nonce|wrapped_urk|kdf_salt)[^,\s"]{0,120}', '${1}=[redacted]')
        $detail = ($detail -replace '\s+', ' ').Trim()
        if ($detail.Length -gt 240) { $detail = $detail.Substring($detail.Length - 240) }
        if ($detail) { Write-Host "DETAIL $detail" }
    }
    return [pscustomobject]@{ Ok = $ok; Exit = $exit; Envelope = $envlp; Stdout = $(if ($Secret) { '' } else { $stdout }); Row = $row; Progress = @($progressSamples.ToArray()) }
}

function Assert-Count([string]$Name, $Envelope, [string]$Field, [int]$Min) {
    $n = 0
    if ($Envelope -and $Envelope.summary -and $null -ne $Envelope.summary.$Field) {
        $n = [int]$Envelope.summary.$Field
    }
    if ($n -lt $Min) { throw "$Name ${Field}=$n, expected at least $Min" }
}

function New-Pass {
    -join ((48..57 + 65..90 + 97..122 | Get-Random -Count 24 | ForEach-Object { [char]$_ }))
}

function Get-SmokeSession([string]$DataRoot, [string]$ExpectedUser) {
    $sessions = @(Get-ChildItem -LiteralPath $DataRoot -Filter session.json -Recurse -File | ForEach-Object {
        $value = Get-Content -LiteralPath $_.FullName -Raw -Encoding utf8 | ConvertFrom-Json
        if ([string]$value.user -eq $ExpectedUser -and $value.token) { [pscustomobject]@{ Path = $_.FullName; Value = $value } }
    })
    if ($sessions.Count -ne 1) { throw 'Expected exactly one disposable session for this device/user.' }
    return $sessions[0]
}

function Test-SmokeProfileRoot([string]$Profile, [string]$DeviceRoot) {
    if (-not [IO.Path]::IsPathFullyQualified($Profile)) { return $false }
    $relative = [IO.Path]::GetRelativePath($DeviceRoot, $Profile)
    return -not [IO.Path]::IsPathRooted($relative) -and $relative -cne '..' -and -not $relative.StartsWith('..' + [IO.Path]::DirectorySeparatorChar)
}

function Add-CloudRegistration([string]$User, [string]$Password) {
    # Record before the request: a failed response does not prove no account was created.
    if ($User -notmatch "^(sweep|oth|sps)$stamp$") { throw 'Registration is outside this sweep namespace.' }
    $account = [pscustomobject]@{ User = $User; Password = $Password; Super = ''; Confirmed = $false; Purged = $false }
    $script:CloudAccounts.Add($account)
    return $account
}

function Confirm-CloudRegistration($Account, $Registration) {
    if ($Registration.Ok -and $Registration.Envelope.summary.ok -eq $true -and
        [string]$Registration.Envelope.summary.user -ceq $Account.User -and
        [string]$Registration.Envelope.summary.addr -ceq $Server -and
        [bool]$Registration.Envelope.summary.super) {
        $Account.Super = [string]$Registration.Envelope.summary.super
        $Account.Confirmed = $true
    }
}

function Invoke-CloudCleanup {
    if ($script:CloudCleanupCompleted -or $script:CloudCleanupRunning) { return }
    $script:CloudCleanupRunning = $true
    $events = New-Object System.Collections.Generic.List[object]
    foreach ($account in $script:CloudAccounts) {
        $event = @{ user = $account.User; passed = $false; old_token_rejected = $false }
        try {
            if (-not $account.Confirmed) { throw 'registration_outcome_unconfirmed' }
            if ($account.User -notmatch "^(sweep|oth|sps)$stamp$") { throw 'cleanup_scope_mismatch' }
            $dataDir = Join-Path $Root "cleanup/$($account.User)"
            $login = Invoke-Om -Name 'cleanup-owned-account-login' -ArgList @('--json', 'login', '--interactive', '--addr', $Server, '--user', $account.User, '--pass', $account.Password, '--super', $account.Super) -DataDir $dataDir -Secret -HostProfile -TimeoutSec 120
            if (-not $login.Ok -or [string]$login.Envelope.summary.user -cne $account.User) { throw 'cleanup_login_failed' }
            $session = Get-SmokeSession $dataDir $account.User
            if ([string]$session.Value.addr -cne $Server) { throw 'cleanup_session_server_mismatch' }
            $headers = @{ Authorization = "Bearer $($session.Value.token)" }
            $response = Invoke-WebRequest -Uri "$Server/api/self/purge" -Method Post -Headers $headers -ContentType 'application/json' -Body (@{ confirm = $account.User } | ConvertTo-Json -Compress) -MaximumRedirection 0 -SkipHttpErrorCheck -TimeoutSec 45
            if ([int]$response.StatusCode -ne 200) { throw 'cleanup_purge_not_confirmed' }
            $result = $response.Content | ConvertFrom-Json
            if ($result.purged -ne $true -or [string]$result.user -cne $account.User) { throw 'cleanup_purge_not_confirmed' }
            $rejected = Invoke-WebRequest -Uri "$Server/api/self" -Method Get -Headers $headers -MaximumRedirection 0 -SkipHttpErrorCheck -TimeoutSec 45
            if ([int]$rejected.StatusCode -ne 401) { throw 'cleanup_old_token_still_accepted' }
            $account.Purged = $true
            $event.passed = $true
            $event.old_token_rejected = $true
        } catch {
            # Use a fixed classification; exception text can include authenticated request details.
            $known = @('registration_outcome_unconfirmed', 'cleanup_scope_mismatch', 'cleanup_login_failed', 'cleanup_session_server_mismatch', 'cleanup_purge_not_confirmed', 'cleanup_old_token_still_accepted')
            $reason = [string]$_.Exception.Message
            $event.reason = if ($reason -cin $known) { $reason } else { 'cleanup_request_failed' }
        }
        $events.Add($event)
    }
    $remaining = @($script:CloudAccounts | Where-Object { -not $_.Purged } | ForEach-Object { $_.User })
    $script:CloudCleanup = @{ passed = $remaining.Count -eq 0; events = @($events.ToArray()); remaining_users = $remaining }
    $script:CloudCleanupCompleted = $true
    $script:CloudCleanupRunning = $false
}
