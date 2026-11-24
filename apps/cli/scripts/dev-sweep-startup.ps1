
$script:UseExe = [bool]$Exe
$script:ExePath = $Exe
if ($script:UseExe) {
    if (-not (Test-Path $script:ExePath)) { throw "binary not found: $($script:ExePath)" }
    Write-Host "ROOT $resolvedRoot"
    Write-Host "EXE $($script:ExePath)"
    $script:CliJs = ''
    $cliVersion = ''
    $script:BinarySha256 = (Get-FileHash -LiteralPath $script:ExePath -Algorithm SHA256).Hash.ToLowerInvariant()
} else {
Write-Host "ROOT $resolvedRoot"
Write-Host "WAIT $Package"
$seen = ''
for ($try = 0; $try -lt 24; $try++) {
    $seen = (npm view $Package version --registry https://registry.npmjs.org 2>$null | Out-String).Trim()
    if ($LASTEXITCODE -eq 0 -and $seen) { break }
    Start-Sleep -Seconds 10
}
if (-not $seen) { throw "package not visible on npm yet: $Package" }
Write-Host "INSTALL $Package ($seen)"
npm install --prefix $Prefix $Package --include=optional --no-fund --no-audit --registry https://registry.npmjs.org
if ($LASTEXITCODE -ne 0) { throw "npm install failed: $LASTEXITCODE" }
$script:CliJs = Join-Path $Prefix 'node_modules\rsrs-cli\bin\cli.js'
if (-not (Test-Path $script:CliJs)) { throw "$script:CliJs not found" }
$pkgJson = Get-Content (Join-Path $Prefix 'node_modules\rsrs-cli\package.json') -Raw | ConvertFrom-Json
$cliVersion = [string]$pkgJson.version
$tags = npm view rsrs-cli dist-tags --json --registry https://registry.npmjs.org | ConvertFrom-Json
$specIsDev = $requested -eq 'dev' -or $requested -match '-dev\.'
if ($specIsDev -and $cliVersion -notmatch '-dev') { throw "Refused: the installed version is not a dev build: $cliVersion" }
if ($requested -ne 'dev' -and $cliVersion -ne $requested) { throw "installed $cliVersion but $requested was requested" }
if ($specIsDev -and [string]$tags.latest -eq $cliVersion) { throw "Refused: dev version $cliVersion already equals latest" }
Write-Host "CLI $cliVersion latest=$($tags.latest) dev=$($tags.dev)"
$rid = [System.Runtime.InteropServices.RuntimeInformation]::RuntimeIdentifier
$plat = switch ($rid) {
    'win-x64' { '@rsrsai/win-x64' }
    'win-arm64' { '@rsrsai/win-arm64' }
    'osx-arm64' { '@rsrsai/macos-arm64' }
    'linux-x64' { '@rsrsai/linux-x64' }
    'linux-arm64' { '@rsrsai/linux-arm64' }
    default { '' }
}
if ($env:RSRS_LIBC -and $rid -like 'linux-*') {
    if ($env:RSRS_LIBC -notin @('glibc', 'musl')) { throw 'RSRS_LIBC must be musl or glibc' }
    if ($plat -and $env:RSRS_LIBC -eq 'glibc') { $plat += '-gnu' }
}
if ($plat) {
    npm install --prefix $Prefix "$plat@$cliVersion" --no-fund --no-audit --registry https://registry.npmjs.org
    if ($LASTEXITCODE -ne 0) { throw "platform package $plat install failed: $LASTEXITCODE" }
}
}

$ver = Invoke-Om -Name 'version' -ArgList @('--version') -DataDir $DirA -Raw -Note $(if ($cliVersion) { $cliVersion } else { $ExpectVersion })
$wantVersion = if ($ExpectVersion) { $ExpectVersion.Trim().TrimStart('v') } else { $cliVersion }
Invoke-Om -Name 'version-short' -ArgList @('-v') -DataDir $DirA -Raw -Note $wantVersion | Out-Null
Invoke-Om -Name 'version-v' -ArgList @('v') -DataDir $DirA -Raw -Note $wantVersion | Out-Null
$verJson = Invoke-Om -Name 'version-json' -ArgList @('--json', 'v') -DataDir $DirA -Note 'command=version'
if (-not ($verJson.Stdout -like '*"command":"version"*')) {
    Add-SweepRow 'version-json-command' $false $verJson.Stdout
}
if ($wantVersion -and $ver.Stdout -notmatch [regex]::Escape($wantVersion)) { throw "version output does not match $wantVersion" }
$script:ObservedVersion = [string]$verJson.Envelope.summary.version
$script:ObservedVersionDisplay = $ver.Stdout.Trim()
Assert-Smoke 'exact-artifact-version' ($ver.Ok -and $verJson.Ok -and $script:ObservedVersion -ceq $wantVersion -and $script:ObservedVersionDisplay -ceq "rsrs $wantVersion")

if (-not $BusinessOnly) {
Invoke-Om -Name 'help' -ArgList @('--help') -DataDir $DirA -Raw | Out-Null
foreach ($path in $script:Catalog.command_paths) {
    $helpArgs = @($path.command -split ' ') + @('--help')
    $commandHelp = Invoke-Om -Name "help-$($path.command.Replace(' ', '-'))" -ArgList $helpArgs -DataDir $DirA -Raw -MinStdout 20
    Assert-Smoke "help-$($path.command.Replace(' ', '-'))-contract" ($commandHelp.Ok -and $commandHelp.Stdout -match 'Usage:')
}
$prompt = Invoke-Om -Name 'prompt' -ArgList @('--json', 'prompt') -DataDir $DirA
Assert-Smoke 'prompt-instructions-present' ($prompt.Ok -and [string]$prompt.Envelope.command -eq 'prompt' -and [string]$prompt.Envelope.summary.instructions -match 'rsrs')
Invoke-Om -Name 'no-args-no-tty' -ArgList @() -DataDir $DirA -ExpectExit @(2) -Raw -Note 'no TTY should exit 2' | Out-Null
Invoke-Om -Name 'parse-error' -ArgList @('not-a-command') -DataDir $DirA -ExpectExit @(2) -Raw | Out-Null

$splitHelp = Invoke-Om -Name 'split-help-contract' -ArgList @('split', '--help') -DataDir $DirA -Raw
if ($splitHelp.Stdout -notmatch 'Split a mixed node' -or $splitHelp.Stdout -match 'Tree hygiene|--parent') {
    throw 'split help contains another command description'
}
$cureHelp = Invoke-Om -Name 'tree-cure-help-contract' -ArgList @('tree-cure', '--help') -DataDir $DirA -Raw
if ($cureHelp.Stdout -notmatch 'Tree hygiene') { throw 'tree-cure help has no description' }
$pluginHelp = Invoke-Om -Name 'plugin-help-contract' -ArgList @('plugin', '--help') -DataDir $DirA -Raw
if ($pluginHelp.Stdout -match '--list' -or $pluginHelp.Stdout -notmatch 'list shows config') {
    throw 'plugin help does not match its subcommands'
}
Invoke-Om -Name 'plugin-reject-list-flag' -ArgList @('plugin', '--list') -DataDir $DirA -ExpectExit @(2) -Raw | Out-Null
Invoke-Om -Name 'plugin-test-requires-event' -ArgList @('plugin', 'test') -DataDir $DirA -ExpectExit @(2) -Raw | Out-Null
Invoke-Om -Name 'chain-reject-from-flag' -ArgList @('chain', '--from', 'missing') -DataDir $DirA -ExpectExit @(2) -Raw | Out-Null
}

Invoke-Om -Name 'config-addr' -ArgList @('--json', 'config', '--addr', $Server) -DataDir $DirA | Out-Null
$cfg = Invoke-Om -Name 'config-read' -ArgList @('--json', 'config') -DataDir $DirA
$cfgAddr = [string]$cfg.Envelope.summary.addr
if ($cfgAddr -ne $Server) { throw "config address is not the test server: $cfgAddr" }
$configWrite = Invoke-Om -Name 'config-isolated-settings' -ArgList @('--json', 'config', '--autosync', 'false', '--cure-auto', 'false', '--rpc-parallelism', '1') -DataDir $DirA
$configRead = Invoke-Om -Name 'config-settings-readback' -ArgList @('--json', 'config') -DataDir $DirA
Assert-Smoke 'config-settings-persisted' ($configWrite.Ok -and $configRead.Ok -and $configRead.Envelope.summary.autosync -eq $false -and $configRead.Envelope.summary.cure_auto -eq $false -and [int]$configRead.Envelope.summary.rpc_parallelism -eq 1)
$bgeInstall = Invoke-Om -Name 'model-install' -ArgList @('--json', 'model', 'install') -DataDir $DirA -TimeoutSec 900
Assert-Smoke 'model-bge-installed' ($bgeInstall.Ok -and [string]$bgeInstall.Envelope.summary.model -eq 'm3' -and (Test-Path -LiteralPath (Join-Path $Root 'models/bge-m3/onnx/model_quantized.onnx')))
# The installer owns the CPU cache per profile; later profiles use this shared model root.
python -c "import sys; sys.path.insert(0, sys.argv[1]); from dev_model_fixture import share_cpu_container; share_cpu_container(sys.argv[2], {'RSRS_DATA_DIR': sys.argv[3]}, sys.argv[4])" $PSScriptRoot $Root $DirA (Join-Path $Root 'models/bge-m3')
if ($LASTEXITCODE -ne 0) { throw 'Shared CPU model fixture preparation failed' }
if (-not $BusinessOnly) {
$offlineDir = Join-Path $Root 'offline'
Invoke-Om -Name 'offline-config' -ArgList @('--json', 'config', '--addr', $Server, '--autosync', 'false') -DataDir $offlineDir | Out-Null
$offlineKeys = Invoke-Om -Name 'keygen-local-success' -ArgList @('--json', 'keygen', '--pass', (New-Pass)) -DataDir $offlineDir -Secret -HostProfile
Assert-Smoke 'keygen-local-v4-material' ($offlineKeys.Ok -and [bool]$offlineKeys.Envelope.summary.super -and (Test-Path -LiteralPath (Join-Path $offlineDir 'session.json')))
Complete-SmokeKeygen 'keygen-local' $offlineKeys $offlineDir
$firstLocalWrap = (Get-Content -LiteralPath (Join-Path $offlineDir 'session.json') -Raw | ConvertFrom-Json).wrapped_urk
$offlineEntry = Invoke-Om -Name 'keygen-local-write' -ArgList @('--json', 'remember', "offline-$stamp", '--title', "offline-$stamp", '--importance', 'important', '--force') -DataDir $offlineDir
$offlineId = [string]$offlineEntry.Envelope.summary.id
$offlineRead = Invoke-Om -Name 'keygen-local-readback' -ArgList @('--json', 'show', $offlineId) -DataDir $offlineDir
Assert-Smoke 'keygen-local-memory-readable' ($offlineEntry.Ok -and $offlineRead.Ok -and [string]$offlineRead.Envelope.details.entry.content -eq "offline-$stamp")
$localRefuse = Invoke-Om -Name 'keygen-local-overwrite-refused' -ArgList @('--json', 'keygen') -DataDir $offlineDir -Secret -AllowStatus @('fail')
Assert-Smoke 'keygen-local-overwrite-guard' ($localRefuse.Ok -and $localRefuse.Exit -eq 1 -and (Get-Content -LiteralPath (Join-Path $offlineDir 'session.json') -Raw | ConvertFrom-Json).wrapped_urk -eq $firstLocalWrap)
$forcedKeys = Invoke-Om -Name 'keygen-force-disposable' -ArgList @('--json', 'keygen', '--force') -DataDir $offlineDir -Secret -HostProfile
Assert-Smoke 'keygen-force-rotates-material' ($forcedKeys.Ok -and [bool]$forcedKeys.Envelope.summary.super -and (Get-Content -LiteralPath (Join-Path $offlineDir 'session.json') -Raw | ConvertFrom-Json).wrapped_urk -ne $firstLocalWrap)
Complete-SmokeKeygen 'keygen-force' $forcedKeys $offlineDir
$offlineLogout = Invoke-Om -Name 'offline-full-logout' -ArgList @('--json', 'logout', '--full') -DataDir $offlineDir -HostProfile
Assert-Smoke 'full-logout-session-file-removed' ($offlineLogout.Ok -and -not (Test-Path -LiteralPath (Join-Path $offlineDir 'session.json')))
}

$user = "sweep$stamp"
$pass = New-Pass
$mainCloudAccount = Add-CloudRegistration $user $pass
[System.IO.File]::WriteAllText((Join-Path $Secrets 'account.txt'), "user=$user`n", [Text.UTF8Encoding]::new($false))
$reg = Invoke-Om -Name 'register' -ArgList @('--json', 'register', '--addr', $Server, '--user', $user, '--pass', $pass) -DataDir $DirA -Secret -TimeoutSec 120
Confirm-CloudRegistration $mainCloudAccount $reg
if (-not $reg.Ok) { throw 'register failed, aborting. The key lives in secrets; never share it.' }
$super = ''
if ($reg.Envelope.summary.super) { $super = [string]$reg.Envelope.summary.super }
if (-not $super) { throw 'register response has no summary.super' }
$regAddr = [string]$reg.Envelope.summary.addr
if ($regAddr -ne $Server) { throw "register address is not the test server: $regAddr" }

# Engine selection is Runtime business RPC and requires the registered fixture's credentials.
$engine = Invoke-Om -Name 'model-engine-cpu' -ArgList @('--json', 'model', 'engine', 'cpu') -DataDir $DirA
Assert-Smoke 'model-engine-cpu-selected' ($engine.Ok -and [string]$engine.Envelope.summary.engine -eq 'cpu')
if (-not $BusinessOnly) {
foreach ($configuredEngine in @('gpu', 'npu')) {
    $savedEngine = Invoke-Om -Name "model-engine-configure-$configuredEngine" -ArgList @('--json', 'model', 'engine', $configuredEngine) -DataDir $DirA
    $engineSettings = Get-Content -LiteralPath (Join-Path $DirA 'inference.json') -Raw -Encoding utf8 | ConvertFrom-Json
    # This asserts saved selection only; it does not claim accelerated inference.
    Assert-Smoke "model-engine-$configuredEngine-selection-persisted" ($savedEngine.Ok -and [string]$engineSettings.engine -eq $configuredEngine -and $engineSettings.force_cpu -eq $false)
}
Invoke-Om -Name 'model-engine-restore-cpu' -ArgList @('--json', 'model', 'engine', 'cpu') -DataDir $DirA | Out-Null
}

# Serving inference requires the explicitly registered fixture's session credentials.
$probe = Invoke-Om -Name 'model-probe-m3' -ArgList @('--json', 'model', 'probe', '--text', 'isolated release smoke') -DataDir $DirA -TimeoutSec 300
Assert-Smoke 'model-m3-default-probe-success' ($probe.Ok -and [string]$probe.Envelope.status -eq 'ok' -and [string]$probe.Envelope.summary.model -eq 'm3' -and [int]$probe.Envelope.summary.dimensions -eq 1024)

$st = Invoke-Om -Name 'status-json' -ArgList @('--json', 'status') -DataDir $DirA
$serverAddr = ''
if ($st.Envelope.summary.server_addr) { $serverAddr = [string]$st.Envelope.summary.server_addr }
if ($serverAddr -ne $Server) { throw "status.server_addr=$serverAddr is not the test server. Aborting." }
if ($BusinessOnly) {
    Assert-Smoke 'business-registration-and-server' ($mainCloudAccount.Confirmed -and $st.Ok -and $st.Row.Status -ceq 'ok' -and (Test-SmokeProfileRoot ([string]$st.Envelope.summary.data_dir) $DirA))
}
if (-not $BusinessOnly) {
Invoke-Om -Name 'status-human' -ArgList @('status') -DataDir $DirA -Raw | Out-Null
$revisionInitial = Invoke-Om -Name 'memory-revision-json' -ArgList @('--json', 'memory-revision') -DataDir $DirA
Assert-Smoke 'memory-revision-profile-token' ($revisionInitial.Ok -and [string]$revisionInitial.Envelope.command -ceq 'memory-revision' -and [string]$revisionInitial.Envelope.summary.profile -ceq [string]$st.Envelope.summary.data_dir -and [string]$revisionInitial.Envelope.summary.revision -cmatch '^[0-9a-f]{32}$')
Invoke-Om -Name 'status-before-revision-repeat' -ArgList @('--json', 'status') -DataDir $DirA | Out-Null
$revisionRepeat = Invoke-Om -Name 'memory-revision-repeat' -ArgList @('--json', 'memory-revision') -DataDir $DirA
Assert-Smoke 'memory-revision-reads-stable' ($revisionRepeat.Ok -and [string]$revisionRepeat.Envelope.command -ceq 'memory-revision' -and [string]$revisionRepeat.Envelope.summary.profile -ceq [string]$revisionInitial.Envelope.summary.profile -and [string]$revisionRepeat.Envelope.summary.revision -ceq [string]$revisionInitial.Envelope.summary.revision)

Invoke-Om -Name 'doctor-human' -ArgList @('doctor') -DataDir $DirA -Raw -ExpectExit @(0, 1, 2) -MinStdout 40 -TimeoutSec 180 -Note 'exit 1 means a check failed; allowed on a clean runner' | Out-Null
$docj = Invoke-Om -Name 'doctor-json' -ArgList @('--json', 'doctor') -DataDir $DirA -TimeoutSec 180 -AllowStatus @('ok', 'warn', 'fail', 'skip') -RequireCommand 'doctor' -Note 'the envelope must be doctor and summary.total must exist'
$mcpBinItem = @($docj.Envelope.items) | Where-Object { [string]$_.name -eq 'mcp bin' } | Select-Object -First 1
$binName = if ($IsWindows) { 'rsrs.exe' } else { 'rsrs' }
$mcpBinPath = Join-Path $DirA "bin\$binName"
Add-SweepRow 'doctor-mcp-bin-item' ($docj.Ok -and [string]$docj.Envelope.command -ceq 'doctor' -and $null -ne $mcpBinItem -and [string]$mcpBinItem.status -ceq 'ok') ([string]$mcpBinItem.value)
Add-SweepRow 'doctor-mcp-bin-path' ($null -ne $mcpBinItem -and [string]$mcpBinItem.value -ceq $mcpBinPath) ([string]$mcpBinItem.value)
Add-SweepRow 'mcp-bin-file' (Test-Path -LiteralPath $mcpBinPath) $mcpBinPath
$embedder = @($docj.Envelope.items) | Where-Object { [string]$_.name -eq 'embedder' } | Select-Object -First 1
if ($embedder -and [string]$embedder.status -eq 'fail') {
    Invoke-Om -Name 'doctor-fix' -ArgList @('--json', 'doctor', '--fix') -DataDir $DirA -TimeoutSec 900 -AllowStatus @('ok', 'warn', 'fail') -RequireCommand 'doctor' -Note 'install BGE on a clean environment' | Out-Null
}
$remoteDoctor = Invoke-Om -Name 'doctor-remote' -ArgList @('--json', 'doctor', '--remote') -DataDir $DirA -TimeoutSec 180 -AllowStatus @('ok', 'warn', 'fail') -RequireCommand 'doctor'
$remoteItem = @($remoteDoctor.Envelope.items | Where-Object { [string]$_.name -eq 'remote' } | Select-Object -First 1)
Assert-Smoke 'doctor-remote-health-reachable' ($remoteDoctor.Ok -and $remoteItem.Count -eq 1 -and [string]$remoteItem[0].status -ceq 'ok' -and [string]$remoteItem[0].value -ceq 'Configured remote /health -> 200')

Invoke-Om -Name 'taxonomy' -ArgList @('--json', 'taxonomy') -DataDir $DirA | Out-Null
Invoke-Om -Name 'taxonomy-list' -ArgList @('--json', 'taxonomy', '--list') -DataDir $DirA | Out-Null
Invoke-Om -Name 'taxonomy-ensure' -ArgList @('--json', 'taxonomy', '--ensure', $TX_DEV) -DataDir $DirA | Out-Null
Invoke-Om -Name 'agent-config' -ArgList @('--json', 'agent-config') -DataDir $DirA | Out-Null
Invoke-Om -Name 'agent-config-set' -ArgList @('--json', 'agent-config', '--set', 'diary_mode=concise') -DataDir $DirA | Out-Null
Invoke-Om -Name 'update-check' -ArgList @('--json', 'update-check') -DataDir $DirA -TimeoutSec 180 | Out-Null
Invoke-Om -Name 'update-check-clear' -ArgList @('--json', 'update-check', '--clear') -DataDir $DirA | Out-Null
$pluginList = Invoke-Om -Name 'plugin-list' -ArgList @('--json', 'plugin', 'list') -DataDir $DirA
if (($pluginList.Envelope.actions -join ' ') -match '--event') { throw 'plugin action suggests unsupported --event' }
Invoke-Om -Name 'plugin-test' -ArgList @('--json', 'plugin', 'test', 'post-recall', '--payload', '{}') -DataDir $DirA | Out-Null
$vetoCommand = if ($IsWindows) { 'echo {"verdict":"block","reason":"sweep veto"}' } else { 'echo ''{"verdict":"block","reason":"sweep veto"}''' }
$pluginConfig = @{ hooks = @{ 'pre-remember' = @(@{ cmd = $vetoCommand; on_error = 'skip'; timeout_ms = 2000 }) } } | ConvertTo-Json -Depth 5
$pluginPath = Join-Path $DirA 'plugins.json'
[IO.File]::WriteAllText($pluginPath, $pluginConfig, [Text.UTF8Encoding]::new($false))
try {
    $veto = Invoke-Om -Name 'plugin-explicit-veto' -ArgList @('--json', 'plugin', 'test', 'pre-remember', '--payload', '{}') -DataDir $DirA -AllowStatus @('warn')
    if (-not $veto.Envelope.summary.blocked -or $veto.Envelope.details.reason -notmatch 'sweep veto') {
        throw 'configured plugin did not execute and return its veto'
    }
} finally {
    Remove-Item -LiteralPath $pluginPath
}
Invoke-Om -Name 'inject-targets' -ArgList @('--json', 'inject', '--targets') -DataDir $DirA | Out-Null
Invoke-Om -Name 'inject-preview-codex' -ArgList @('--json', 'inject', '--preview', '--id', 'codex') -DataDir $DirA -ExpectExit @(0, 1, 2) -Raw -Note 'preview only, do not write local agent config' | Out-Null
Invoke-Om -Name 'inject-tui-no-tty' -ArgList @('inject', '--tui') -DataDir $DirA -ExpectExit @(1, 2) -Raw -Note 'no TTY' | Out-Null
}

# Disable autosync so the later explicit sync owns the push step.
Invoke-Om -Name 'config-autosync-off-before-write' -ArgList @('--json', 'config', '--autosync', 'false') -DataDir $DirA | Out-Null
if (-not $BusinessOnly) {
$revisionBeforeWrite = Invoke-Om -Name 'memory-revision-before-write' -ArgList @('--json', 'memory-revision') -DataDir $DirA
}
$marker = "sweep-marker-$stamp"
$title = "sweep-marker-$stamp"
$rem = Invoke-Om -Name 'remember-force' -ArgList @(
    '--json', 'remember', "dev sweep marker $marker on TB16P",
    '--title', $title, '--type', 'decision', '--importance', 'important', '--force', '--project', 'respire-cli'
) -DataDir $DirA -TimeoutSec 300
$id = ''
if ($rem.Envelope.summary.id) { $id = [string]$rem.Envelope.summary.id }
if (-not $id) { throw 'remember returned no summary.id, the extraction chain cannot continue.' }
if ($BusinessOnly) {
    Assert-Smoke 'business-remember-created' ($rem.Ok -and $rem.Row.Status -ceq 'ok' -and [bool]$id)
}
