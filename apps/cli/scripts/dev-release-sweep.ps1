#Requires -Version 7
<#
.SYNOPSIS
  Sweep the npm dev CLI against the development server using isolated data.
  Run only in GitHub Actions; the default port would disrupt a developer runtime.
  Authenticate shutdown of the previous test runtime before switching libraries.
.DESCRIPTION
  Use disposable accounts. Keep keys in the isolated secrets directory, never reports.
  Usage: pwsh -NoProfile -File scripts/dev-release-sweep.ps1
#>
[CmdletBinding()]
param(
    [string]$Server = $env:RSRS_DEV_SERVER_ADDR,
    [string]$Package = 'rsrs-cli@dev',
    [string]$Root = '',
    [string]$Exe = '',
    [string]$ExpectVersion = '',
    [switch]$DeferSupplemental,
    [switch]$BusinessOnly
)

$ErrorActionPreference = 'Stop'

# The taxonomy parent and the cause/action/result markers are built from code points so this
# script stays pure ASCII while still producing the exact values the store expects.
$TX_DEV = [string]::Join('', @(0x7F16, 0x7A0B, 0x5F00, 0x53D1 | ForEach-Object { [char]$_ }))
$MARK_CAUSE = [string]::Join('', @(0x3010, 0x524D, 0x56E0, 0x3011 | ForEach-Object { [char]$_ }))
$MARK_ACTION = [string]::Join('', @(0x3010, 0x884C, 0x4E3A, 0x3011 | ForEach-Object { [char]$_ }))
$MARK_RESULT = [string]::Join('', @(0x3010, 0x540E, 0x679C, 0x3011 | ForEach-Object { [char]$_ }))
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Release smoke may run only in GitHub Actions.' }
if ($Server -cne 'https://api-dev.rsrs.rs') { throw 'Release smoke requires exactly https://api-dev.rsrs.rs.' }
if (-not $Exe -or $ExpectVersion -notmatch '^\d+\.\d+\.\d+(-dev\.\d+)?$') {
    throw 'Release smoke requires an exact CI artifact via -Exe and its -ExpectVersion.'
}
if ($env:CLI_SHA -notmatch '^[0-9a-fA-F]{40}$') { throw 'CLI_SHA must identify the verified artifact source commit.' }
if ([string]::IsNullOrWhiteSpace($env:RSRS_DEV_SERVER_ADDR) -or $Server -ne $env:RSRS_DEV_SERVER_ADDR) {
    throw 'Configure RSRS_DEV_SERVER_ADDR with the approved development server before running this sweep.'
}
$pkgMatch = [regex]::Match($Package, '^rsrs-cli@(dev|\d+\.\d+\.\d+(-dev\.\d+)?)$')
if (-not $pkgMatch.Success) {
    throw "Refused: the package must be rsrs-cli@dev or rsrs-cli@x.y.z[-dev.n], got $Package"
}
$requested = $pkgMatch.Groups[1].Value

$stamp = "$([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())$([Guid]::NewGuid().ToString('N').Substring(0, 8))"
if (-not $Root) {
    if ($env:RUNNER_TEMP) {
        $Root = Join-Path $env:RUNNER_TEMP "om-dev-sweep-$stamp"
    } elseif (Test-Path 'E:\tmp_rust_target') {
        $Root = "E:\tmp_rust_target\om-dev-sweep-$stamp"
    } else {
        $Root = Join-Path ([System.IO.Path]::GetTempPath()) "om-dev-sweep-$stamp"
    }
}
$resolvedRoot = [System.IO.Path]::GetFullPath($Root)
if ($resolvedRoot -match '(?i)[/\\]\.(onememory|respire)([/\\]|$)') {
    throw "Refused: the root sits inside the user store .onememory/.respire: $resolvedRoot"
}
$Root = $resolvedRoot
New-Item -ItemType Directory -Force -Path $Root, (Join-Path $Root 'logs'), (Join-Path $Root 'secrets'), (Join-Path $Root 'work') | Out-Null
$Prefix = Join-Path $Root 'npm'
$DirA = Join-Path $Root 'a'
$DirB = Join-Path $Root 'b'
$DirD = Join-Path $Root 'd'
$Work = Join-Path $Root 'work'
$Secrets = Join-Path $Root 'secrets'
$ReportPath = Join-Path $Root 'report.md'
$CoveragePath = Join-Path $Root 'coverage.json'
$SmokeHome = Join-Path $Root 'home'
New-Item -ItemType Directory -Force -Path $SmokeHome | Out-Null
if ($IsMacOS) {
    # The native keyring reads user-domain preferences under the CLI's isolated HOME.
    # Configure only this disposable home; never replace the runner's default keychain.
    $preferences = Join-Path $SmokeHome 'Library/Preferences'
    $keychains = Join-Path $SmokeHome 'Library/Keychains'
    New-Item -ItemType Directory -Force -Path $preferences, $keychains | Out-Null
    $keychain = Join-Path $keychains 'rsrs-smoke.keychain-db'
    function Invoke-SmokeSecurity([string[]]$SecurityArgs) {
        $output = & /usr/bin/env "HOME=$SmokeHome" /usr/bin/security @SecurityArgs 2>&1
        if ($LASTEXITCODE -ne 0) { throw "Isolated keychain setup failed: $($SecurityArgs[0])" }
        return ($output -join "`n")
    }
    Invoke-SmokeSecurity @('create-keychain', '-p', '', $keychain) | Out-Null
    Invoke-SmokeSecurity @('list-keychains', '-d', 'user', '-s', $keychain) | Out-Null
    Invoke-SmokeSecurity @('default-keychain', '-d', 'user', '-s', $keychain) | Out-Null
    Invoke-SmokeSecurity @('unlock-keychain', '-p', '', $keychain) | Out-Null
    Invoke-SmokeSecurity @('set-keychain-settings', '-t', '3600', '-u', $keychain) | Out-Null
    $selectedKeychain = (Invoke-SmokeSecurity @('default-keychain', '-d', 'user')).Trim().Trim('"')
    if ($selectedKeychain -cne $keychain) { throw 'Isolated keychain default readback does not match the fixture.' }
}
$script:Catalog = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dev-smoke-cli-coverage.json') -Raw -Encoding utf8 | ConvertFrom-Json
if ($DeferSupplemental) {
    $approved = @('runtime', 'ai_inject', 'outbox', 'legacy_vault')
    $actual = @($script:Catalog.supplemental_required | Sort-Object -Unique)
    if (@($script:Catalog.required_remaining).Count -gt 0 -or @($actual | Where-Object { $_ -notin $approved }).Count -gt 0 -or $actual.Count -ne $approved.Count -or @($script:Catalog.supplemental_required).Count -ne $approved.Count) {
        throw 'Only the four explicitly assigned supplemental suites may be deferred; unknown required functionality must fail closed.'
    }
}
. (Join-Path $PSScriptRoot 'dev-sweep-redacted-reports.ps1')
. (Join-Path $PSScriptRoot 'dev-sweep-owned-runtime.ps1')
. (Join-Path $PSScriptRoot 'dev-sweep-startup.ps1')
. (Join-Path $PSScriptRoot 'dev-sweep-account.ps1')

$syncA = Invoke-Om -Name 'sync-a' -ArgList @('--json', 'sync') -DataDir $DirA -TimeoutSec 180
if (-not $syncA.Ok) { throw 'A sync failed' }
Assert-Count 'A sync' $syncA.Envelope 'pushed' 1
if ($BusinessOnly) {
    Assert-Smoke 'business-upload-confirmed' ($syncA.Ok -and $syncA.Row.Status -ceq 'ok' -and [int]$syncA.Envelope.summary.pushed -gt 0)
}
if (-not $BusinessOnly) {
Invoke-Om -Name 'sync-human' -ArgList @('sync') -DataDir $DirA -Raw -TimeoutSec 180 | Out-Null
Invoke-Om -Name 'sync-conflicts' -ArgList @('--json', 'sync-conflicts') -DataDir $DirA -TimeoutSec 120 | Out-Null
Invoke-Om -Name 'sync-conflicts-all' -ArgList @('--json', 'sync-conflicts', '--all') -DataDir $DirA -TimeoutSec 120 | Out-Null
$hist = Invoke-Om -Name 'sync-history' -ArgList @('--json', 'sync-history', '--id', $short) -DataDir $DirA -TimeoutSec 120
Invoke-Om -Name 'sync-history-remote' -ArgList @('--json', 'sync-history', '--id', $short, '--remote') -DataDir $DirA -TimeoutSec 120 | Out-Null
$restoreFixture = Invoke-Om -Name 'sync-restore-fixture' -ArgList @('--json', 'remember', "restore original $stamp", '--title', "restore-$stamp", '--parent', $id, '--importance', 'important', '--force') -DataDir $DirA
$restoreId = [string]$restoreFixture.Envelope.summary.id
Assert-Smoke 'sync-restore-fixture-id' ($restoreFixture.Ok -and [bool]$restoreId)
$restoreHistory = Invoke-Om -Name 'sync-restore-local-history' -ArgList @('--json', 'sync-history', '--id', $restoreId) -DataDir $DirA
$localRevision = @($restoreHistory.Envelope.details | Where-Object { $_.op_id -and [string]$_.entry.content -eq "restore original $stamp" } | Select-Object -First 1)
Assert-Smoke 'sync-restore-local-revision-exists' ($restoreHistory.Ok -and $localRevision.Count -eq 1)
Invoke-Om -Name 'sync-restore-fixture-edit' -ArgList @('--json', 'update', $restoreId, '--content', "restore changed $stamp") -DataDir $DirA | Out-Null
$restoreOp = Invoke-Om -Name 'sync-restore-op-id' -ArgList @('--json', 'sync-restore', '--op-id', ([string]$localRevision[0].op_id)) -DataDir $DirA -AllowStatus @('pending')
$restoreRead = Invoke-Om -Name 'sync-restore-op-readback' -ArgList @('--json', 'show', $restoreId) -DataDir $DirA
Assert-Smoke 'sync-restore-op-content-persisted' ($restoreOp.Ok -and [string]$restoreOp.Envelope.summary.restored -eq $restoreId -and $restoreRead.Ok -and [string]$restoreRead.Envelope.details.entry.content -eq "restore original $stamp")
Invoke-Om -Name 'sync-restore-upload' -ArgList @('--json', 'sync') -DataDir $DirA | Out-Null
$remoteHistory = Invoke-Om -Name 'sync-restore-remote-history' -ArgList @('--json', 'sync-history', '--id', $restoreId, '--remote') -DataDir $DirA
$remoteRevision = @($remoteHistory.Envelope.details | Where-Object { $_.epoch -and $null -ne $_.rev -and [string]$_.entry.id -eq $restoreId -and [string]$_.entry.content -eq "restore original $stamp" } | Select-Object -First 1)
Assert-Smoke 'sync-restore-remote-revision-exists' ($remoteHistory.Ok -and $remoteRevision.Count -eq 1)
Invoke-Om -Name 'sync-restore-remote-fixture-edit' -ArgList @('--json', 'update', $restoreId, '--content', "restore remote changed $stamp") -DataDir $DirA | Out-Null
$restoreRemote = Invoke-Om -Name 'sync-restore-epoch-rev' -ArgList @('--json', 'sync-restore', '--epoch', ([string]$remoteRevision[0].epoch), '--rev', ([string]$remoteRevision[0].rev)) -DataDir $DirA -AllowStatus @('pending')
$remoteRead = Invoke-Om -Name 'sync-restore-remote-readback' -ArgList @('--json', 'show', $restoreId) -DataDir $DirA
Assert-Smoke 'sync-restore-remote-content-persisted' ($restoreRemote.Ok -and $remoteRead.Ok -and [string]$remoteRead.Envelope.details.entry.content -eq "restore original $stamp")
Invoke-Om -Name 'sync-resolve-bogus' -ArgList @(
    '--json', 'sync-resolve', '--epoch', '0', '--rev', '0', '--head-rev', '0', '--action', 'keep-current'
) -DataDir $DirA -AllowStatus @('fail') -Note 'must fail when there is no conflict' | Out-Null
}

Invoke-Om -Name 'config-addr-b' -ArgList @('--json', 'config', '--addr', $Server) -DataDir $DirB | Out-Null
$loginB = Invoke-Om -Name 'login-b' -ArgList @('--json', 'login', '--interactive', '--addr', $Server, '--user', $user, '--pass', $pass, '--super', $super) -DataDir $DirB -Secret -HostProfile -TimeoutSec 180
if (-not $loginB.Ok) { throw 'B login failed. super must come from the register response summary.super. Aborted so later commands do not hit the default production server.' }
$stB = Invoke-Om -Name 'status-b' -ArgList @('--json', 'status') -DataDir $DirB
$addrB = ''
if ($stB.Envelope.summary.server_addr) { $addrB = [string]$stB.Envelope.summary.server_addr }
if ($addrB -ne $Server) { throw "B server_addr=$addrB" }
if ($BusinessOnly) {
    $profileB = [string]$loginB.Envelope.summary.dir
    $statusProfileB = [string]$stB.Envelope.summary.data_dir
    $sameProfileB = [IO.Path]::IsPathFullyQualified($statusProfileB) -and [IO.Path]::IsPathFullyQualified($profileB) -and [IO.Path]::GetFullPath($statusProfileB) -ceq [IO.Path]::GetFullPath($profileB)
    Assert-Smoke 'business-independent-login' ($loginB.Ok -and $loginB.Row.Status -ceq 'ok' -and $loginB.Envelope.summary.ok -eq $true -and [string]$loginB.Envelope.summary.user -ceq $user -and [string]$loginB.Envelope.summary.addr -ceq $Server -and (Test-SmokeProfileRoot $profileB $DirB) -and $sameProfileB -and $stB.Ok -and $stB.Row.Status -ceq 'ok')
}
$syncB = Invoke-Om -Name 'sync-b' -ArgList @('--json', 'sync') -DataDir $DirB -TimeoutSec 180
if (-not $syncB.Ok) { throw 'B download sync failed' }
Assert-Smoke 'device-b-sync-converged' ($syncB.Envelope.summary.converged -eq $true -and [int]$syncB.Envelope.summary.pending -eq 0)
$recB = Invoke-Om -Name 'recall-b' -ArgList @('--json', 'recall', $marker, '--limit', '3') -DataDir $DirB -TimeoutSec 180 -AllowIndexWait
if ($recB.Stdout -notlike "*$marker*") { throw "B recall did not return the marker $marker written by A" }
if ($BusinessOnly) {
    Assert-Smoke 'business-download-converged' ($syncB.Row.Status -ceq 'ok' -and $syncB.Envelope.summary.converged -eq $true -and [int]$syncB.Envelope.summary.pending -eq 0)
    $businessHits = @($recB.Envelope.details | Where-Object { [string]$_.entry.id -ceq $id -and [string]$_.entry.content -ceq "dev sweep marker $marker on TB16P" })
    Assert-Smoke 'business-cross-device-recall' ($recB.Ok -and $recB.Row.Status -ceq 'ok' -and $businessHits.Count -gt 0)
    Invoke-CloudCleanup
    if (-not $script:CloudCleanup.passed -or @($script:CloudCleanup.remaining_users).Count -gt 0) { throw 'Disposable account cleanup was not confirmed.' }
    if (@($Rows | Where-Object { -not $_.Ok -or $_.Status -eq 'skip' }).Count -gt 0) { throw 'Business diagnostics contain failed or skipped steps.' }
    $script:BusinessStatus = 'passed'
    Write-SweepReport -Force
    Stop-OurRuntimes
    exit 0
}
. (Join-Path $PSScriptRoot 'dev-sweep-memory.ps1')
. (Join-Path $PSScriptRoot 'dev-sweep-sync.ps1')
$lines.Add("") | Out-Null
$lines.Add("- time: $([DateTimeOffset]::UtcNow.ToString('o'))") | Out-Null
$lines.Add("- package: $Package") | Out-Null
$lines.Add("- version: $cliVersion") | Out-Null
$lines.Add("- server: $Server") | Out-Null
$lines.Add("- latest: $($tags.latest)") | Out-Null
$lines.Add("- directory: $resolvedRoot") | Out-Null
$lines.Add("- passed: $($passed.Count)  failed: $($failed.Count)") | Out-Null
$lines.Add("") | Out-Null
$lines.Add("| step | command | exit | status | result | note |") | Out-Null
$lines.Add("| --- | --- | --- | --- | --- | --- |") | Out-Null
foreach ($row in $Rows) {
    $mark = if (-not $row.Ok) { 'FAIL' } elseif ($row.Status -eq 'fail') { 'EXPECTED_ERROR' } elseif ($row.Status -eq 'skip') { 'SKIP' } else { 'PASS' }
    $note = Get-SafeReportNote (($row.Note + ' ' + $row.Leak).Trim())
    $lines.Add("| $($row.Step) | $($row.Name) | $($row.Exit) | $($row.Status) | $mark | $note |") | Out-Null
}
[System.IO.File]::WriteAllText($ReportPath, ($lines -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
Write-Host "REPORT $ReportPath"
Write-Host "PASS $($passed.Count) FAIL $($failed.Count)"
Stop-OurRuntimes
if (-not $script:CloudCleanup.passed) {
    Write-Host 'FAILED: disposable cloud account cleanup was not confirmed; see coverage.json.'
    exit 1
}
if ($failed.Count -gt 0) { exit 1 }
if (@($script:Catalog.required_remaining).Count -gt 0) {
    Write-Host 'INCOMPLETE: required functional coverage remains; see coverage.json.'
    exit 1
}
if (@($script:Catalog.supplemental_required).Count -gt 0 -and -not $DeferSupplemental) {
    Write-Host 'INCOMPLETE: supplemental suites must pass the combined release gate; see coverage.json.'
    exit 1
}
exit 0
