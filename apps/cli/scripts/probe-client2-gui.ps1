[CmdletBinding(DefaultParameterSetName="Official")]
param(
    [Parameter(Mandatory)][string]$Cli,
    [Parameter(Mandatory)][string]$RuntimeArchive,
    [Parameter(Mandatory)][string]$RuntimeVersion,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [switch]$OfflineAcceptance,
    [string]$OfflineModelDirectory,
    [Parameter(Mandatory,ParameterSetName='CoreQuery')][switch]$CoreConfigQuery,
    [Parameter(Mandatory,ParameterSetName='CoreQuery')][string]$CoreQueryInputs,
    [Parameter(Mandatory,ParameterSetName='CoreQuery')][string]$SealedClient2Directory,
    [Parameter(Mandatory,ParameterSetName='SourceCandidate')][switch]$SourceCandidate,
    [Parameter(Mandatory,ParameterSetName='SourceCandidate')][string]$SourceClient2Exe,
    [Parameter(Mandatory,ParameterSetName='SourceCandidate')][string]$SourceClient2Manifest,
    [Parameter(Mandatory,ParameterSetName='SourceCandidate')][string]$SourceClient2Commit,
    [Parameter(Mandatory,ParameterSetName='Official')][string]$Client2Release,
    [Parameter(Mandatory,ParameterSetName='DesktopCandidate')][string]$Client2CandidateRunId,
    [Parameter(Mandatory,ParameterSetName='Official')][Parameter(Mandatory,ParameterSetName='DesktopCandidate')][Parameter(Mandatory,ParameterSetName='CoreQuery')][string]$Client2Commit
)
$ErrorActionPreference = 'Stop'
$publishedAssets = $env:ARTIFACT_SOURCE -ceq 'published-assets'
$client2Candidate = $SourceCandidate -or $PSCmdlet.ParameterSetName -ceq 'DesktopCandidate'
$publishedOnline = $publishedAssets -and -not $client2Candidate
$candidateAssets = $env:ARTIFACT_SOURCE -ceq 'build' -and -not [string]::IsNullOrEmpty($env:CANDIDATE_ASSETS)
$candidate = if ($candidateAssets) { $env:CANDIDATE_ASSETS | ConvertFrom-Json }
$fullFixture = $publishedOnline -or $candidateAssets
if ($PSCmdlet.ParameterSetName -ne 'SourceCandidate' -and $Client2Commit -cnotmatch '\A[0-9a-f]{40}\z') { throw 'Require the exact protocol-compatible Client2 source commit' }
if ($PSCmdlet.ParameterSetName -eq 'Official' -and $Client2Release -cnotmatch '\Av[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?\z') { throw 'Require an explicit new protocol-compatible Client2 release' }
if ($publishedAssets -and (-not $OfflineAcceptance -or
    $env:RSRS_ARTIFACT_SOURCE -cne 'published-assets' -or $env:REQUEST_PUBLISH -cne 'false' -or
    $env:GITHUB_EVENT_NAME -cne 'workflow_dispatch')) { throw 'Online installation requires verified published assets and the official offline subset' }
$report = [ordered]@{
    scope = 'protocol-compatible Client2 Windows GUI subset; not full GUI acceptance'
    source_sha = $Client2Commit
    release = $Client2Release
    environment_ready = $false; official_exe_verified = $false
    native_window_visible = $false; isolated_webview_bound = $false
    native_accessibility_read = $false; foreground_verified = $false; gui_input_observed = $false
    target_desktop_screenshot_saved = $false
    cleanup_exit_confirmed = $false; cleanup_pending_absence = @()
    cli_processes = @(); cli_observation_diagnostics = @()
    full_gui_acceptance = 'not-evaluated'; g1_passed = $false; status = 'failed'
    cloud_login = 'not-evaluated-no-gui-account-fixture'
}
if ($PSCmdlet.ParameterSetName -ceq 'SourceCandidate' -and -not $SourceCandidate) { throw 'Source candidate mode must be explicitly enabled' }
if ($SourceCandidate) {
    $report.scope='source Client2 Windows GUI subset; not official asset or release acceptance'
    foreach ($key in @('release','asset_id','source_sha','expected_exe_sha256')) { $report.Remove($key) }
    $report.fixture_mode='source-client2-ui-candidate'; $report.source_candidate_verified=$false
}
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:RUNNER_OS -ne 'Windows') {
    throw 'This probe is restricted to GitHub-hosted Windows; do not run on a host desktop'
}
$root = [IO.Path]::GetFullPath($OutputDirectory)
$tempRoot = [IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd('\') + '\'
if (-not $root.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $root)) {
    throw 'Require a new output directory strictly inside RUNNER_TEMP'
}
New-Item -ItemType Directory -Path $root | Out-Null
$desktopProcess = $null
$accountFixture = $null
$coreProxy = $null
$coreRuntimeRecord = $null
$tracked = @()
function Assert-SourceClient2Manifest($Manifest, [string]$ExpectedCommit, [string]$RunId, [string]$RunAttempt, [string]$Job) {
    $keys=@('schema_version','mode','repository','source_commit','platform','exe_file','exe_sha256','build_version','build_command','run_id','run_attempt','job')
    $actual=@($Manifest.PSObject.Properties.Name)
    if ($actual.Count -ne $keys.Count -or @($actual | Where-Object { $_ -cnotin $keys }).Count -ne 0) { throw 'Source candidate manifest schema differs' }
    foreach ($key in $keys | Where-Object { $_ -ne 'schema_version' }) {
        if ($Manifest.$key -isnot [string] -or -not $Manifest.$key) { throw 'Source candidate manifest fields must be strings' }
    }
    if ($Manifest.schema_version -isnot [int] -and $Manifest.schema_version -isnot [long]) { throw 'Source candidate manifest version is not integer' }
    if ($Manifest.schema_version -ne 1 -or $Manifest.mode -cne 'source-client2-ui-candidate' -or
        $Manifest.repository -cne 'risense-ai/respire-client2' -or $Manifest.platform -cne 'windows-amd64' -or
        $Manifest.exe_file -cne 'respire-windows-amd64.exe' -or
        $ExpectedCommit -cnotmatch '\A[0-9a-f]{40}\z' -or $Manifest.source_commit -cne $ExpectedCommit -or
        $Manifest.exe_sha256 -cnotmatch '\A[0-9a-f]{64}\z') { throw 'Source candidate identity differs' }
    if ($Manifest.build_version -cnotmatch '\A[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?\z' -or
        $Manifest.build_command -cne ('./scripts/build.ps1 -Version ' + $Manifest.build_version + ' -Arch amd64')) { throw 'Source candidate build command differs' }
    if ($RunId -cnotmatch '\A[1-9][0-9]*\z' -or $RunAttempt -cnotmatch '\A[1-9][0-9]*\z' -or
        $Job -cnotmatch '\A[A-Za-z0-9_-]+\z' -or $Manifest.run_id -cne $RunId -or
        $Manifest.run_attempt -cne $RunAttempt -or $Manifest.job -cne $Job) { throw 'Source candidate must be built in this Actions job' }
}
function Assert-CandidateRegularPath([string]$Path, [switch]$File) {
    $full=[IO.Path]::GetFullPath($Path)
    if ($full -notmatch '\A[A-Za-z]:\\' -or $full.Substring(2).Contains(':')) { throw 'Source candidate requires a local filesystem path' }
    $inside=$false
    foreach ($allowed in @($env:GITHUB_WORKSPACE,$env:RUNNER_TEMP)) {
        if ($allowed) {
            $prefix=[IO.Path]::GetFullPath($allowed).TrimEnd('\')+'\'
            if ($full.StartsWith($prefix,[StringComparison]::OrdinalIgnoreCase)) { $inside=$true }
        }
    }
    if (-not $inside) { throw 'Source candidate path is outside isolated CI roots' }
    $item=Get-Item -LiteralPath $full -Force -ErrorAction Stop
    if ($File -and $item.PSIsContainer) { throw 'Source candidate requires a regular file' }
    while ($null -ne $item) {
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Source candidate paths may not contain reparse points' }
        $parent=Split-Path -Parent $item.FullName
        if (-not $parent -or $parent -eq $item.FullName) { break }
        $item=Get-Item -LiteralPath $parent -Force -ErrorAction Stop
    }
    return $full
}
function Copy-SourceClient2Candidate([string]$ExePath, [string]$ManifestPath, [string]$ExpectedCommit, [string]$Destination) {
    $source=Assert-CandidateRegularPath $ExePath -File
    $manifestFile=Assert-CandidateRegularPath $ManifestPath -File
    if ((Get-Item -LiteralPath $manifestFile).Length -gt 65536) { throw 'Source candidate manifest exceeds its bound' }
    $manifest=Get-Content -LiteralPath $manifestFile -Raw | ConvertFrom-Json
    Assert-SourceClient2Manifest $manifest $ExpectedCommit $env:GITHUB_RUN_ID $env:GITHUB_RUN_ATTEMPT $env:GITHUB_JOB
    if ([IO.Path]::GetFileName($source) -cne $manifest.exe_file -or
        (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant() -cne $manifest.exe_sha256) { throw 'Source candidate executable bytes differ' }
    $null=Assert-CandidateRegularPath (Split-Path -Parent $Destination)
    if (Test-Path -LiteralPath $Destination) { throw 'Source candidate destination must be new' }
    Copy-Item -LiteralPath $source -Destination $Destination
    $null=Assert-CandidateRegularPath $Destination -File
    if ((Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant() -cne $manifest.exe_sha256) { throw 'Copied source candidate bytes differ' }
    return $manifest
}
function Assert-DesktopClient2Candidate($Run, $Artifact, [string]$ExpectedCommit, [string]$RunId) {
    if ($RunId -cnotmatch '\A[1-9][0-9]*\z' -or $Run.id.ToString() -cne $RunId -or
        $Run.head_sha -cne $ExpectedCommit -or $Run.repository.full_name -cne 'risense-ai/respire-client2' -or
        $Run.head_repository.full_name -cne 'risense-ai/respire-client2' -or
        $Run.path -cne '.github/workflows/desktop.yml' -or $Run.event -cne 'pull_request' -or
        $Run.status -cne 'completed' -or $Run.conclusion -cne 'success' -or
        $Artifact.workflow_run.id.ToString() -cne $RunId -or $Artifact.workflow_run.head_sha -cne $ExpectedCommit -or
        $Artifact.name -cne 'respire-windows-amd64' -or $Artifact.expired -ne $false -or
        $Artifact.id -le 0 -or $Artifact.size_in_bytes -le 0 -or $Artifact.digest -cnotmatch '\Asha256:[0-9a-f]{64}\z') {
        throw 'Client2 Desktop candidate provenance differs'
    }
}
. (Join-Path $PSScriptRoot 'probe-client2-launch-assets.ps1')
try {
    . (Join-Path $PSScriptRoot 'probe-client2-launch-setup.ps1')
    . (Join-Path $PSScriptRoot 'probe-client2-launch-runtime.ps1')
    foreach ($phase in $phases) {
        $recoveryPhase = $phase.StartsWith('recovery-', [StringComparison]::Ordinal)
        if ($phase -ceq 'recovery-issue') {
            # Preserve the completed offline/restart proof. The account fixture uses
            # a fresh profile; issue and resume share this exact profile and WebView.
            $originalBin = Join-Path $taskProfile 'bin'
            $originalPreferences = Get-Content -Raw -LiteralPath $preferencesPath | ConvertFrom-Json
            $taskHome = Join-Path $root 'recovery-profile/home'
            $taskProfile = Join-Path $taskHome '.rsrs'
            foreach ($dir in @('client2','appdata','local','temp')) {
                New-Item -ItemType Directory -Path (Join-Path $taskProfile $dir) | Out-Null
            }
            Copy-Item -LiteralPath $originalBin -Destination (Join-Path $taskProfile 'bin') -Recurse
            $preferencesPath = Join-Path $taskProfile 'client2/preferences.json'
            $originalPreferences.autoRuntime = $false
            $originalPreferences | ConvertTo-Json | Set-Content -LiteralPath $preferencesPath -Encoding utf8NoBOM
            $childEnv['HOME'] = $taskHome; $childEnv['USERPROFILE'] = $taskHome
            $childEnv['APPDATA'] = Join-Path $taskProfile 'appdata'; $childEnv['LOCALAPPDATA'] = Join-Path $taskProfile 'local'
            $childEnv['TEMP'] = Join-Path $taskProfile 'temp'; $childEnv['TMP'] = $childEnv['TEMP']
            $childEnv['RSRS_DATA_DIR'] = $taskProfile; $childEnv['RSRS_BIN_DIR'] = Join-Path $taskProfile 'bin'
            $managed = Join-Path $taskProfile "bin/runtime/$RuntimeVersion"
            $accountReceipt = Join-Path $root 'account-fixture-counts.json'
            $serverStart = [Diagnostics.ProcessStartInfo]::new((Get-Command python -CommandType Application | Select-Object -First 1).Source)
            $serverStart.UseShellExecute = $false; $serverStart.CreateNoWindow = $true
            $serverStart.RedirectStandardOutput = $true; $serverStart.RedirectStandardError = $true
            $serverStart.Environment.Clear()
            foreach ($key in $childEnv.Keys) { $serverStart.Environment[$key] = $childEnv[$key] }
            $serverStart.Environment['GITHUB_ACTIONS'] = 'true'; $serverStart.Environment['RUNNER_ENVIRONMENT'] = 'github-hosted'
            foreach ($arg in @((Join-Path $PSScriptRoot 'dev-login-interruption-smoke.py'),'--gui-recovery-server','--receipt',$accountReceipt)) {
                [void]$serverStart.ArgumentList.Add($arg)
            }
            $accountFixture = [Diagnostics.Process]::Start($serverStart)
            $ready = $accountFixture.StandardOutput.ReadLineAsync()
            if (-not $ready.Wait(10000)) { throw 'Loopback account fixture did not become ready' }
            $accountAddress = ($ready.GetAwaiter().GetResult() | ConvertFrom-Json).addr
            if ($accountFixture.HasExited -or $accountAddress -cnotmatch '\Ahttp://127\.0\.0\.1:[0-9]+\z') { throw 'Loopback account fixture failed' }
            $report.recovery = @{status='failed'; fixture='loopback synthetic account; real CLI setup and GUI reset'; exit_method='guarded-process-termination'}
            # Prepare the account with the released CLI, not another GUI scenario.
            # Registration output includes real material: drain bytes without decoding or saving.
            $recoveryStarted = [DateTime]::UtcNow
            $registration = Child-Start (Join-Path $taskProfile 'bin/rsrs.exe') @('--json','--host-rpc-stdio') -CaptureOutput -CaptureInput
            try {
                $discardOut = $registration.StandardOutput.BaseStream.CopyToAsync([IO.Stream]::Null)
                $discardErr = $registration.StandardError.BaseStream.CopyToAsync([IO.Stream]::Null)
                $registration.StandardInput.WriteLine((@{v=1;id=('desktop-'+[guid]::NewGuid());method='auth.register';
                    params=@{addr=$accountAddress;user='synthetic';password='gui-synthetic-registration-password'}} | ConvertTo-Json -Compress))
                $registration.StandardInput.Close()
                if (-not $registration.WaitForExit(180000)) { $registration.Kill($true); throw 'Real CLI account fixture registration timed out' }
                $discardOut.GetAwaiter().GetResult(); $discardErr.GetAwaiter().GetResult()
                $report.recovery.registration_exit_code = $registration.ExitCode
                if ($registration.ExitCode -ne 0) { throw 'Real CLI account fixture registration failed; private output was not retained' }
            } finally {
                if (-not $registration.HasExited) { $registration.Kill($true); $registration.WaitForExit() }
                $registration.Dispose()
            }
        }
        $cliObservation = [hashtable]::Synchronized(@{stop=$false; desktop_pid=0; desktop_started=0L; phase=$phase})
        $cliObserver = Start-ThreadJob -ArgumentList $cliObservation,(Join-Path $taskProfile 'bin/rsrs.exe'),$report.cli_sha256,$report.environment.session_id -ScriptBlock {
            param($state,$expectedPath,$expectedHash,$session)
            $ErrorActionPreference = 'Stop'
            $seen = [Collections.Generic.HashSet[string]]::new()
            while (-not $state.stop) {
                foreach ($process in [Diagnostics.Process]::GetProcessesByName('rsrs')) {
                    try {
                        if ($state.desktop_pid -eq 0 -or $process.HasExited) { continue }
                        $birth = $process.StartTime.ToUniversalTime().Ticks
                        if ($birth -lt $state.desktop_started -or $process.SessionId -ne $session) { continue }
                        $image = $process.Path
                        if (-not [string]::Equals($image,$expectedPath,[StringComparison]::OrdinalIgnoreCase)) { continue }
                        $record = Get-CimInstance Win32_Process -Filter "ProcessId=$($process.Id)"
                        if (-not $record -or $record.ParentProcessId -ne $state.desktop_pid) { continue }
                        $digest = (Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash.ToLowerInvariant()
                        if ($process.HasExited -or $process.StartTime.ToUniversalTime().Ticks -ne $birth) { throw 'CLI exited during image sampling' }
                        if ($digest -cne $expectedHash -or $record.SessionId -ne $session -or
                            -not [string]::Equals($record.ExecutablePath,$image,[StringComparison]::OrdinalIgnoreCase)) { throw 'Running GUI CLI image differs' }
                        if ($seen.Add("$($process.Id):$birth")) {
                            @{phase=$state.phase;pid=$process.Id;parent_pid=$record.ParentProcessId;started_ticks=$birth.ToString();image=$image;sha256=$digest
                                parent_verified=$true;session_verified=$true;lifetime_verified=$true}
                        }
                    } catch { @{phase=$state.phase;observation_error=$_.Exception.Message} }
                    finally { $process.Dispose() }
                }
                Start-Sleep -Milliseconds 20
            }
        }
        Write-Output ('GUI probe: starting desktop ' + $phase)
        $desktopProcess = Child-Start $exe @()
        $desktopStarted = $desktopProcess.StartTime.ToUniversalTime().Ticks
        $cliObservation.desktop_started = $desktopStarted
        $cliObservation.desktop_pid = $desktopProcess.Id
        Write-Output 'GUI probe: desktop started'
        $nativeRoot = Get-CimInstance Win32_Process -Filter "ProcessId=$($desktopProcess.Id)"
        if (-not $nativeRoot -or -not [string]::Equals($nativeRoot.ExecutablePath, $exe, [StringComparison]::OrdinalIgnoreCase)) { throw 'Native process does not name the verified original EXE' }
        $report.desktop = @{pid=$desktopProcess.Id; session_id=$desktopProcess.SessionId; path=$nativeRoot.ExecutablePath; started_ticks=$desktopStarted}
        if ($desktopProcess.SessionId -ne $report.environment.session_id) { throw 'Desktop launched into a different session' }
        $windows = @()
        for ($attempt=0; $attempt -lt 80; $attempt++) {
            if ($desktopProcess.HasExited) { throw 'Official desktop exited before a native window appeared' }
            $windows = @([RespireGuiNative]::Windows($desktopProcess.Id))
            if (@($windows | Where-Object { $_.visible -and $_.rect_ok -and $_.width -gt 0 -and $_.height -gt 0 -and $_.cloak_query_ok -and $_.cloaked -eq 0 }).Count) { break }
            Start-Sleep -Milliseconds 250
        }
        $report.windows = $windows
        $report.native_window_visible = @($windows | Where-Object { $_.visible -and $_.rect_ok -and $_.width -gt 0 -and $_.height -gt 0 -and $_.cloak_query_ok -and $_.cloaked -eq 0 }).Count -gt 0
        if (-not $report.native_window_visible) { throw 'No visible uncloaked original native window' }
        $visible = @($windows | Where-Object { $_.visible -and $_.rect_ok -and $_.width -gt 0 -and $_.height -gt 0 -and $_.cloak_query_ok -and $_.cloaked -eq 0 })
        if ($visible.Count -ne 1) { throw 'Require one unambiguous original visible window' }
        $currentTree = @(Descendants $desktopProcess.Id)
        $tracked = @($tracked) + $currentTree
        $report.processes = @($tracked | Select-Object ProcessId,ParentProcessId,ExecutablePath,CommandLine,CreationDate,SessionId)
        $expectedProfile = [IO.Path]::GetFullPath((Join-Path $taskProfile 'client2/webview2/EBWebView'))
        $profileArg = '--user-data-dir=(?:"' + [regex]::Escape($expectedProfile) + '"|' + [regex]::Escape($expectedProfile) + '(?=\s|$))'
        $browser = @($currentTree | Where-Object { [IO.Path]::GetFileName($_.ExecutablePath) -eq 'msedgewebview2.exe' -and $_.CommandLine -match $profileArg -and $_.CommandLine -notmatch '(?:^|\s)--type=' })
        if ($browser.Count -ne 1 -or $browser[0].SessionId -ne $desktopProcess.SessionId) { throw 'Original WebView browser/profile/session binding failed' }
        $report.isolated_webview_bound = $true
        $report.native_binding = @{
            desktop_pid=$desktopProcess.Id; desktop_started_ticks=$desktopStarted; exe=$exe; exe_sha256=$report.exe_sha256
            session_id=$desktopProcess.SessionId; hwnd=$visible[0].hwnd
            browser_pid=$browser[0].ProcessId; browser_started_ticks=(Get-Process -Id $browser[0].ProcessId).StartTime.ToUniversalTime().Ticks
            webview_profile=$expectedProfile; full_gui_acceptance='not-evaluated'; g1_passed=$false
            rpc_port=$rpcPort; data_directory=$taskProfile; preferences_path=$preferencesPath
            final_public_assets=$publishedOnline
            verified_candidate_assets=$candidateAssets
            runtime_exe=(Join-Path $managed 'rsrs-runtime.exe')
            runtime_sha256=(Get-FileHash -LiteralPath (Join-Path $managed 'rsrs-runtime.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        if ($client2Candidate) { $report.native_binding.fixture_mode='source-client2-ui-candidate'; $report.native_binding.source_commit=if ($SourceCandidate) { $SourceClient2Commit } else { $Client2Commit } }
        if ($phase -eq 'restart') {
            $report.native_binding.restart_memory = $report.initial_native_gui.offline.restart_memory
            $report.native_binding.previous_runtime_pid = $report.initial_native_gui.offline.runtime_process.ProcessId
            $report.native_binding.previous_runtime_ticks = $report.initial_native_gui.offline.runtime_process.creation_utc_ticks
        }
        if ($recoveryPhase) {
            $report.native_binding.account_fixture_addr = $accountAddress
            $report.native_binding.recovery_phase = $phase
            if ($phase -in @('recovery-resume','recovery-verified')) { $report.native_binding.recovery_request = $report.recovery.request }
        }
        $bindingPath = Join-Path $root 'native-binding.json'
        if ($CoreConfigQuery) {
            $report.native_binding.core_query = @{phase=$phase; address=$queryProxy.address}
            $report.native_binding.cli_exe = Join-Path $taskProfile 'bin/rsrs.exe'
            $report.native_binding.cli_sha256 = $report.cli_sha256
        }
        $driverBinding = @{}
        foreach ($key in $report.native_binding.Keys) { $driverBinding[$key] = $report.native_binding[$key] }
        if ($CoreConfigQuery) {
            # Private binding/control files are never selected by the upload.
            # Keep control credentials out of the report's native_binding.
            $driverBinding.core_query = @{phase=$phase; address=$queryProxy.address; control_file=$readyPath}
        }
        $driverBinding | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $bindingPath -Encoding utf8NoBOM
        Write-Output 'GUI probe: window and WebView bound'
        $systemPowerShell = [IO.Path]::GetFullPath((Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'))
        $helperPath = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot 'probe-client2-gui-native.ps1')).Path
        $helperArgs = @('-NoLogo','-NoProfile','-NonInteractive','-Mta','-File',$helperPath,'-BindingPath',$bindingPath,'-OutputDirectory',$root)
        if ($OfflineAcceptance) { $helperArgs += '-OfflineAcceptance' }
        if ($phase -eq 'restart') { $helperArgs += '-ExistingKeys' }
        if ($recoveryPhase) { $helperArgs += @('-RecoveryPhase', $phase.Substring('recovery-'.Length)) }
        if ($client2Candidate) { $helperArgs += '-SourceCandidateStability' }
        if ($CoreConfigQuery) { $helperArgs += '-CoreConfigQuery' }
        Write-Output 'GUI probe: starting UI Automation helper'
        $driver = Child-Start $systemPowerShell $helperArgs
        Write-Output 'GUI probe: UI Automation helper started'
        $helperDeadline = if ($OfflineAcceptance -or $CoreConfigQuery) { 360000 } else { 45000 }
        if (-not $driver.WaitForExit($helperDeadline)) {
            Write-Output 'GUI probe: UI Automation helper timed out; stopping helper'
            $driver.Kill($true); throw 'System UI Automation helper timed out'
        }
        Write-Output 'GUI probe: UI Automation helper exited'
        $native = Get-Content -Raw -LiteralPath (Join-Path $root 'native-gui.json') | ConvertFrom-Json
        if ($recoveryPhase) { $report.recovery_native_gui = $native } else { $report.native_gui = $native }
        if ($client2Candidate -and ($native.fixture_mode -cne 'source-client2-ui-candidate' -or
            $native.offline.account_focus_stability.status -cne 'timed-window-passed' -or
            $native.offline.account_focus_stability.completed_poll_count -cne 'not-observed')) { throw 'Source candidate focus stability was not verified' }
        if ($OfflineAcceptance -and -not $recoveryPhase -and ($native.offline.status -ne 'offline-subset-passed' -or
            -not $native.offline.runtime_normal_stop_confirmed -or $native.offline.full_gui_acceptance -ne 'not-evaluated' -or
            $native.offline.g1_passed -ne $false)) { throw 'Original offline GUI subset failed; full GUI remains not-evaluated' }
        $report.native_accessibility_read = $native.accessibility_tree_read -eq $true
        $report.foreground_verified = $native.foreground_verified -eq $true
        $report.gui_input_observed = $native.native_tab_sent -eq $true -and $native.focus_changed_in_target_document -eq $true
        $report.target_desktop_screenshot_saved = $native.target_desktop_screenshot_saved -eq $true
        if ($driver.ExitCode -ne 0 -or $native.status -ne 'native-connectivity-passed' -or -not $native.identity_verified -or
            -not $report.native_accessibility_read -or -not $report.foreground_verified -or -not $report.gui_input_observed -or
            -not $report.target_desktop_screenshot_saved -or $native.full_gui_acceptance -ne 'not-evaluated' -or $native.g1_passed -ne $false) { throw 'Original native GUI connectivity probe failed; see native-gui.json' }
        if ($CoreConfigQuery) {
            if ($native.core_query.status -cne ($phase + '-passed')) { throw 'Core query phase failed' }
            Copy-Item -LiteralPath (Join-Path $root 'native-gui.json') -Destination (Join-Path $root "$phase.json")
            if ($phase -ceq 'core-issue') {
                $report.core_query_original = $native.core_query.transaction
            } elseif (($native.core_query.transaction | ConvertTo-Json -Compress -Depth 6) -cne
                ($report.core_query_original | ConvertTo-Json -Compress -Depth 6)) { throw 'Restart changed RequestBound identity' }
        }
        $cliObservation.stop = $true
        $null = Wait-Job -Job $cliObserver -Timeout 10
        if ($cliObserver.State -cne 'Completed') { throw 'CLI image observer did not finish' }
        $observations = @(Receive-Job -Job $cliObserver -ErrorAction Stop)
        Remove-Job -Job $cliObserver
        $cliObserver = $null
        $report.cli_observation_diagnostics = @($report.cli_observation_diagnostics) + @($observations | Where-Object { $_.observation_error })
        if (@($observations | Where-Object { $_.observation_error -ceq 'Running GUI CLI image differs' }).Count) { throw 'Observed GUI CLI image identity mismatch' }
        $verifiedProcesses = @($observations | Where-Object { $_.lifetime_verified -eq $true })
        $report.cli_processes = @($report.cli_processes) + $verifiedProcesses
        if ($fullFixture -and $verifiedProcesses.Count -eq 0) { throw 'No live GUI-owned CLI image was verified; see observer diagnostics' }
        if ($CoreConfigQuery -and $phase -ceq 'core-issue') {
            $tracked = @($tracked) + @(Descendants $desktopProcess.Id)
            foreach ($item in $tracked) { Stop-TrackedOwned $item }
            foreach ($item in $tracked) { if (-not (Confirm-TrackedAbsent $item)) { throw 'Original desktop survived core query restart' } }
            $tracked = @()
        }
        if ($publishedOnline) {
            $persistedPreferences = Get-Content -Raw -LiteralPath $preferencesPath | ConvertFrom-Json
            $report.online_install.runtime_channel = $persistedPreferences.runtimeChannel
            if ($persistedPreferences.runtimeChannel -cne 'dev') { throw 'Fresh product preferences did not retain the default dev discovery channel' }
        }
        if ($fullFixture -and $OfflineAcceptance) {
            if (-not $recoveryPhase -and -not $native.offline.recall_hit) { throw 'Published Client2 recall was not verified' }
            if ($phase -eq 'initial') {
                $report.initial_native_gui = $native
                Copy-Item -LiteralPath (Join-Path $root 'native-gui.json') -Destination (Join-Path $root 'native-gui-initial.json')
                $tracked = @($tracked) + @(Descendants $desktopProcess.Id)
                # Guarded process termination tests restart after desktop exit, not tray-menu shutdown.
                foreach ($item in $tracked) { Stop-TrackedOwned $item }
                foreach ($item in $tracked) {
                    if (-not (Confirm-TrackedAbsent $item)) { throw 'Original desktop tree survived before restart' }
                }
                if (Get-CimInstance Win32_Process -Filter "ProcessId=$($native.offline.runtime_process.ProcessId)") { throw 'Original Runtime survived before desktop restart' }
                # Every recorded actor is now confirmed absent; do not carry its PID into a later phase.
                $tracked = @()
                $report.restart = @{exit_method='guarded-process-termination'; original_tree_absent=$true; completed=$false}
            } elseif ($phase -ceq 'restart') {
                if (-not $native.offline.automatic_start -or -not $native.offline.automatic_key_handoff) { throw 'Published desktop automatic start/key handoff was not verified' }
                $report.restart.completed = $true
                $report.restart_native_gui = $native
            } else {
                if ($native.recovery.status -cne 'recovery-subset-passed' -or
                    ($phase -cne 'recovery-verified' -and -not $native.recovery.unchecked_blocked)) {
                    throw 'Real reset recovery acknowledgement gate was not verified'
                }
                $counts = Get-Content -Raw -LiteralPath $accountReceipt | ConvertFrom-Json
                if ($counts.registers -ne 1 -or $counts.logins -ne 0 -or $counts.posts -ne 2) {
                    throw 'CLI fixture registration and GUI reset did not publish exactly once each'
                }
                if ($phase -ceq 'recovery-issue') {
                    $report.recovery.request = $native.recovery.request
                    $report.recovery.counts_before_exit = $counts
                    $report.recovery.issue = $native.recovery
                } elseif ($phase -ceq 'recovery-resume') {
                    if (-not $native.recovery.material_saved) { throw 'Recovery confirmation was not persisted' }
                    $report.recovery.resume = $native.recovery
                    $report.recovery.original_operation_not_repeated = $true
                } else {
                    if (-not $native.recovery.saved_restart_no_prompt) { throw 'Saved acknowledgement was not verified after another restart' }
                    $report.recovery.saved_restart = $native.recovery
                    $report.recovery.status = 'recovery-subset-passed'
                }
            }
            if ($phase -in @('restart','recovery-issue','recovery-resume')) {
                $tracked = @($tracked) + @(Descendants $desktopProcess.Id)
                foreach ($item in $tracked) { Stop-TrackedOwned $item }
                foreach ($item in $tracked) {
                    if (-not (Confirm-TrackedAbsent $item)) { throw 'Desktop tree survived before account recovery restart' }
                }
                # The absence barrier above retires only this completed phase's actor records.
                $tracked = @()
                if ($phase -ceq 'recovery-issue') { $report.recovery.unacknowledged_tree_absent = $true }
            }
        }
    }
    $report.status = 'connectivity-passed'
} catch {
    $report.error = $_.Exception.Message
} finally {
    if ($cliObserver) {
        try {
            $cliObservation.stop = $true
            $null = Wait-Job -Job $cliObserver -Timeout 10
            if ($cliObserver.State -ne 'Completed') { Stop-Job -Job $cliObserver }
            $report.cli_observation_diagnostics = @($report.cli_observation_diagnostics) + @(Receive-Job -Job $cliObserver -ErrorAction Stop)
            Remove-Job -Job $cliObserver
        } catch {
            $report.cli_observer_cleanup_error = $_.Exception.Message
            $report.cleanup_error = 'CLI image observer cleanup failed'
            $report.status = 'failed'
        }
    }
    # Stop only the original process tree observed in this fresh hosted VM, with creation-time guards.
    try {
        Write-Output 'GUI probe: cleaning up tracked processes'
        if ($desktopProcess) {
            if ($report.native_binding.recovery_phase) {
                # This fresh account profile has its own verified Runtime image path.
                # Include a detached actor only with the same image, session and birth scope.
                $actors = @(Get-CimInstance Win32_Process -Filter "Name='rsrs-runtime.exe'" | Where-Object {
                    [string]::Equals($_.ExecutablePath,$report.native_binding.runtime_exe,[StringComparison]::OrdinalIgnoreCase)
                })
                foreach ($actor in $actors) {
                    if ($actor.SessionId -ne $desktopProcess.SessionId -or $actor.CreationDate.ToUniversalTime() -lt $recoveryStarted) {
                        throw 'Account fixture Runtime cleanup identity is outside the original scope'
                    }
                    $tracked += $actor
                }
            }
            if ($OfflineAcceptance -and $report.native_binding) {
              try {
                $checkpointPath = Join-Path $root 'native-gui.json'
                if (-not (Test-Path -LiteralPath $checkpointPath)) { throw 'No detached Runtime identity checkpoint; cleanup cannot be certified' }
                $checkpoint = Get-Content -Raw -LiteralPath $checkpointPath | ConvertFrom-Json
                if ($checkpoint.offline.runtime_process) {
                    $runtimeRecord = $checkpoint.offline.runtime_process
                    if (-not [string]::Equals($runtimeRecord.ExecutablePath,$report.native_binding.runtime_exe,[StringComparison]::OrdinalIgnoreCase) -or
                        $runtimeRecord.SessionId -ne $desktopProcess.SessionId) { throw 'Detached cleanup record is not the bound staged Runtime' }
                    $runtimeRecord.CreationDate = Read-CheckpointCreationDate $runtimeRecord
                    if ($runtimeRecord.CreationDate.ToUniversalTime().Ticks -lt $desktopStarted) { throw 'Detached actor predates the original desktop' }
                    $tracked += $runtimeRecord
                } elseif ($checkpoint.offline -and -not $report.native_binding.recovery_phase) { $report.cleanup_error = 'Offline Runtime identity not established; detached cleanup is not certified' }
              } catch { $report.cleanup_error = $_.Exception.Message }
            }
            $sameRoot = Get-Process -Id $desktopProcess.Id -ErrorAction SilentlyContinue
            if ($sameRoot -and $sameRoot.StartTime.ToUniversalTime().Ticks -eq $desktopStarted) {
                $tracked = @($tracked) + @(Descendants $desktopProcess.Id)
                $report.processes_before_cleanup = @($tracked | Select-Object ProcessId,ParentProcessId,ExecutablePath,CommandLine,CreationDate,SessionId)
            }
            foreach ($item in $tracked) {
                try { Stop-TrackedOwned $item } catch { $report.cleanup_error = $_.Exception.Message }
            }
            foreach ($item in $tracked) {
                if (-not (Confirm-TrackedAbsent $item)) { throw "Owned process survived cleanup: $($item.ProcessId)" }
            }
            $report.cleanup_exit_confirmed = -not $report.cleanup_error
        }
    } catch { $report.cleanup_error = $_.Exception.Message }
    if ($accountFixture) {
        try {
            if (-not $accountFixture.HasExited) { $accountFixture.Kill($true); $accountFixture.WaitForExit() }
        } catch { $report.cleanup_error = 'Loopback account fixture cleanup failed'; $report.cleanup_exit_confirmed = $false }
        $accountFixture.Dispose()
    }
    if ($CoreConfigQuery) {
        if ($report.status -ceq 'connectivity-passed') {
            try {
                $finalQuery = Invoke-RestMethod -Uri ($queryProxy.address + '/__fixture') -Headers @{'X-Fixture-Control'=$queryProxy.nonce} -TimeoutSec 10
                $publicKeys = @('operation_id','apply_ids','result_ids','result_issued','result_forwarded','result_response_lost','dropped_apply_replies','errors','core_test_mode','scope','apply_fusion','effective_fusion')
                $actualKeys = @($finalQuery.PSObject.Properties.Name)
                if ($actualKeys.Count -ne $publicKeys.Count -or @($actualKeys | Where-Object { $_ -cnotin $publicKeys }).Count) { throw 'Forwarding proof has non-public fields' }
                if (@($finalQuery.errors).Count -or @($finalQuery.apply_ids).Count -ne 1 -or $finalQuery.dropped_apply_replies -ne 1 -or
                    $finalQuery.apply_fusion -cne 'attention' -or $finalQuery.effective_fusion -cne 'attention' -or
                    $finalQuery.result_issued -ne @($finalQuery.result_ids).Count -or $finalQuery.result_forwarded -ne $finalQuery.result_issued -or
                    $finalQuery.result_response_lost -lt 1 -or $finalQuery.result_response_lost -ge $finalQuery.result_forwarded -or
                    @($finalQuery.result_ids).Count -lt 2 -or @($finalQuery.result_ids | Where-Object { $_ -cne $report.core_query_original.operation_id }).Count) { throw 'Final forwarding evidence is not one original apply with original-only result queries' }
                $report.core_query_final_counts = $finalQuery
            } catch { $report.core_query_evidence_error = $_.Exception.Message; $report.status = 'failed' }
        }
        try {
            if (-not $coreRuntimeRecord) { throw 'Real Runtime cleanup identity was not established' }
            $currentRuntime = Get-CimInstance Win32_Process -Filter "ProcessId=$($coreRuntimeRecord.ProcessId)"
            if (-not $currentRuntime -or -not (Test-TrackedCreationIdentity $currentRuntime $coreRuntimeRecord)) { throw 'Real Runtime identity changed before drain' }
            $live = Invoke-RestMethod -Uri "http://127.0.0.1:$directPort/api/health" -Headers @{'X-Respire-Health'='runtime-v1'} -TimeoutSec 10
            foreach ($key in @('pid','process_start_id','image_sha256','installation_id','account_id','instance_id','generation')) {
                if ($live.$key -cne $directHealth.$key) { throw 'Real Runtime owner changed before drain' }
            }
            $stopBody = @{installation_id=$live.installation_id;instance_id=$live.instance_id;account_id=$live.account_id;generation=$live.generation}
            $stopped = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$directPort/api/runtime/stop" -ContentType 'application/json' -Headers @{'X-Respire-Lifecycle'='coordinator-v1'} -Body ($stopBody | ConvertTo-Json -Compress) -TimeoutSec 10
            if (-not $stopped.ok) { throw 'Real Runtime drain request failed' }
            $draining = Get-Process -Id $coreRuntimeRecord.ProcessId -ErrorAction SilentlyContinue
            if ($draining -and -not $draining.WaitForExit(10000)) { throw 'Real Runtime drain did not exit' }
            if (-not (Confirm-TrackedAbsent $coreRuntimeRecord)) { throw 'Real Runtime survived drain' }
            $receipt = Get-Content -Raw -LiteralPath (Join-Path $taskProfile "runtime/stop-$($live.instance_id).json") | ConvertFrom-Json
            if ($receipt.state -cne 'drained' -or @($receipt.unfinished).Count) { throw 'Real Runtime drain has unfinished work' }
            foreach ($key in @('pid','process_start_id','image_sha256','installation_id','account_id','instance_id','generation')) {
                if ($receipt.$key -cne $live.$key) { throw 'Real Runtime stop receipt identity differs' }
            }
            $report.core_query_drain_verified = $true
        } catch { $report.cleanup_error = $_.Exception.Message; $report.cleanup_exit_confirmed = $false }
        finally {
            if ($coreProxy) {
                try {
                    if (-not $coreProxy.HasExited) { $coreProxy.Kill(); $coreProxy.WaitForExit() }
                    $coreProxy.Dispose()
                } catch { $report.cleanup_error = 'Real forwarding helper cleanup failed'; $report.cleanup_exit_confirmed = $false }
            }
        }
    }
    Save-Report
}
if ($report.status -ne 'connectivity-passed' -or -not $report.cleanup_exit_confirmed -or $report.cleanup_error) { exit 1 }
exit 0
