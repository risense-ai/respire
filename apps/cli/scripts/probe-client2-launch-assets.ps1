function Save-SealedClient2([string]$Destination) {
    $selected = $candidate.client2
    $seal = $selected.sealed
    if (-not $OfflineAcceptance -or $PSCmdlet.ParameterSetName -cne 'Official' -or
        $selected.source_sha -cne $Client2Commit -or $selected.release -cne $Client2Release) {
        throw 'Sealed Client2 requires matching full offline candidate inputs'
    }
    if (-not $env:GH_TOKEN) { throw 'Client2 seal verification requires an authenticated GitHub read token' }
    $reader = Join-Path $env:GITHUB_WORKSPACE '.client2-seal-verifier/scripts/desktop-candidate.py'
    $directory = Join-Path $root 'sealed-client2'
    $verified = @'
import json, os, runpy, sys
from pathlib import Path
reader, root, run, attempt, source, version = sys.argv[1:]
os.environ.update(GITHUB_REPOSITORY="risense-ai/respire-client2", INPUT_RUN_ID=run,
                  INPUT_ATTEMPT=attempt, CANDIDATE_SOURCE=source, CANDIDATE_VERSION=version)
os.chdir(Path(root).parent)
runpy.run_path(reader)["producer"](Path(root))
'@ | python - $reader $directory $seal.run_id $seal.run_attempt $Client2Commit $Client2Release.Substring(1)
    if ($LASTEXITCODE) { throw 'Sealed Client2 producer verification failed' }
    $verified = $verified | ConvertFrom-Json
    if ($verified.artifact_id -ne $seal.artifact_id -or $verified.artifact_zip_digest -cne $seal.artifact_zip_digest) {
        throw 'Client2 package seal differs from the selected artifact'
    }
    $manifestPath = Join-Path $directory 'candidate.json'
    $manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
    $filename = 'respire-windows-amd64.exe'
    $expected = $manifest.files.$filename
    Copy-Item -LiteralPath (Join-Path $directory $filename) -Destination $Destination
    $hash = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant()
    if ((Get-Item -LiteralPath $Destination).Length -ne $expected.size -or $hash -cne $expected.sha256) {
        throw 'Copied sealed Client2 executable differs'
    }
    $report.exe_sha256 = $hash
    $report.source_candidate_verified = $true
    $report.sealed_client2 = @{
        repository = $verified.producer.repository; source_sha = $manifest.source; version = $manifest.version
        run_id = $seal.run_id; run_attempt = $seal.run_attempt
        artifact_id = $verified.artifact_id; artifact_zip_digest = $verified.artifact_zip_digest
        manifest_sha256 = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
        exe_sha256 = $hash; six_package_receipts_verified = $true
    }
}
function Save-DownloadedClient2([string]$Destination) {
    if (-not $env:GH_TOKEN) { throw 'Client2 download requires an authenticated GitHub read token' }
    $headers = @{Authorization="Bearer $env:GH_TOKEN"; Accept='application/vnd.github+json'}
    $api = 'https://api.github.com/repos/risense-ai/respire-client2'
    if ($Client2CandidateRunId) {
        $run = Invoke-RestMethod -Headers $headers -Uri "$api/actions/runs/$Client2CandidateRunId" -TimeoutSec 60
        $inventory = Invoke-RestMethod -Headers $headers -Uri "$api/actions/runs/$Client2CandidateRunId/artifacts?per_page=100" -TimeoutSec 60
        $matches = @($inventory.artifacts | Where-Object { $_.name -ceq 'respire-windows-amd64' })
        if ($inventory.total_count -ne $inventory.artifacts.Count -or $matches.Count -ne 1) { throw 'Client2 Desktop candidate inventory is incomplete or ambiguous' }
        $artifact = $matches[0]
        Assert-DesktopClient2Candidate $run $artifact $Client2Commit $Client2CandidateRunId
        $zip = Join-Path $root 'client2-desktop.zip'
        Invoke-WebRequest -Headers $headers -Uri "$api/actions/artifacts/$($artifact.id)/zip" -OutFile $zip -TimeoutSec 120
        if ((Get-Item -LiteralPath $zip).Length -ne $artifact.size_in_bytes -or
            ('sha256:'+(Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()) -cne $artifact.digest) { throw 'Client2 Desktop artifact digest differs' }
        $archive = [IO.Compression.ZipFile]::OpenRead($zip)
        try {
            $entries = @($archive.Entries | Where-Object { $_.FullName -ceq 'respire-windows-amd64.exe' })
            if ($entries.Count -ne 1) { throw 'Client2 Desktop artifact must contain exactly one executable' }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entries[0], $Destination, $false)
        } finally { $archive.Dispose() }
        $report.fixture_mode = 'source-client2-ui-candidate'
        $report.source_candidate_verified = $true
        $report.source_candidate = @{source_commit=$Client2Commit; run_id=$Client2CandidateRunId; run_attempt=$run.run_attempt
            artifact_id=$artifact.id; artifact_digest=$artifact.digest; build_version="0.2.56-dev.$($run.run_number)"}
        $report.Remove('release')
    } else {
        $release = Invoke-RestMethod -Headers $headers -Uri "$api/releases/tags/$Client2Release" -TimeoutSec 60
        $commit = Invoke-RestMethod -Headers $headers -Uri "$api/commits/$Client2Release" -TimeoutSec 60
        $matches = @($release.assets | Where-Object { $_.name -ceq 'respire-windows-amd64.exe' })
        if ($release.draft -ne $false -or $release.tag_name -cne $Client2Release -or
            $commit.sha -cne $Client2Commit -or $matches.Count -ne 1) { throw 'Published Client2 release/source identity differs' }
        $asset = $matches[0]
        if ($asset.id -le 0 -or $asset.size -le 0 -or $asset.digest -cnotmatch '\Asha256:[0-9a-f]{64}\z') { throw 'Published Client2 asset identity is missing' }
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $Destination -TimeoutSec 120
        if ((Get-Item -LiteralPath $Destination).Length -ne $asset.size -or
            ('sha256:'+(Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant()) -cne $asset.digest) { throw 'Published Client2 executable digest differs' }
        $report.asset_id = $asset.id
        $report.expected_exe_sha256 = $asset.digest.Substring(7)
        $report.official_exe_verified = $true
    }
    $report.exe_sha256 = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant()
}
function Save-Report { $report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $root 'report.json') -Encoding utf8NoBOM }
function Descendants([int]$parentId) {
    $all = @(Get-CimInstance Win32_Process)
    $ids = [Collections.Generic.HashSet[int]]::new(); [void]$ids.Add($parentId)
    do {
        $added = $false
        foreach ($item in $all) { if ($ids.Contains([int]$item.ParentProcessId) -and $ids.Add([int]$item.ProcessId)) { $added = $true } }
    } while ($added)
    @($all | Where-Object { $ids.Contains([int]$_.ProcessId) })
}
# Checkpoint dates from Windows PowerShell 5 JSON lose sub-millisecond precision.
# Only a decimal string preserves the full CIM UTC birth identity across hosts.
function Read-CheckpointCreationDate($Record) {
    $text = $Record.creation_utc_ticks
    if ($text -isnot [string] -or $text -cnotmatch '\A[1-9][0-9]{0,18}\z') { throw 'Detached cleanup requires exact creation ticks' }
    $ticks = [long]0
    if (-not [long]::TryParse($text, [Globalization.NumberStyles]::None, [Globalization.CultureInfo]::InvariantCulture, [ref]$ticks) -or
        $ticks -gt [datetime]::MaxValue.Ticks) { throw 'Detached cleanup creation ticks out of range' }
    return [datetime]::new($ticks, [DateTimeKind]::Utc)
}
function Test-TrackedCreationIdentity($Current, $Expected) {
    if ($Current.CreationDate -isnot [datetime] -or $Expected.CreationDate -isnot [datetime]) { throw 'Cleanup process birth evidence is not typed' }
    return $Current.CreationDate.ToUniversalTime().Ticks -eq $Expected.CreationDate.ToUniversalTime().Ticks
}
function Confirm-TrackedAbsent($Item) {
    for ($probe=0; $probe -lt 2; $probe++) {
        $current = Get-CimInstance Win32_Process -Filter "ProcessId=$($Item.ProcessId)"
        if ($current) {
            if (-not (Test-TrackedCreationIdentity $current $Item)) { throw "Process identity changed during cleanup: $($Item.ProcessId)" }
            return $false
        }
        if ($probe -eq 0) { Start-Sleep -Milliseconds 100 }
    }
    return $true
}
function Stop-TrackedOwned($Item) {
    $identityChanged = $false
    try {
        $current = Get-CimInstance Win32_Process -Filter "ProcessId=$($Item.ProcessId)"
        if ($current) {
            if (-not (Test-TrackedCreationIdentity $current $Item)) { $identityChanged = $true; throw "Process identity changed before cleanup: $($Item.ProcessId)" }
            $owned = Get-Process -Id $Item.ProcessId -ErrorAction Stop
            $null = $owned.SafeHandle
            $held = Get-CimInstance Win32_Process -Filter "ProcessId=$($Item.ProcessId)"
            if ($held) {
                if (-not (Test-TrackedCreationIdentity $held $Item)) { $identityChanged = $true; throw "Process identity changed before cleanup: $($Item.ProcessId)" }
                Stop-Process -InputObject $owned -ErrorAction Stop
                if (-not $owned.WaitForExit(5000)) { throw "Owned process did not exit: $($Item.ProcessId)" }
            }
        }
    } catch {
        # A process can exit between CIM/Get-Process/Stop-Process. Confirm absence twice;
        # never clear an observed birth mismatch, or kill a reused PID.
        if ($identityChanged) { throw }
        if (-not (Confirm-TrackedAbsent $Item)) {
            if ($_.FullyQualifiedErrorId -cne 'NoProcessFoundForGivenId,Microsoft.PowerShell.Commands.GetProcessCommand') { throw }
            # Only a vanished Get-Process lookup can defer to the unchanged final
            # two-sample absence gate; a survivor or reused PID still fails there.
            $report.cleanup_pending_absence = @($report.cleanup_pending_absence) + @(@{
                pid=$Item.ProcessId; creation_utc_ticks=$Item.CreationDate.ToUniversalTime().Ticks.ToString([Globalization.CultureInfo]::InvariantCulture)
            })
        }
        return
    }
    if (-not (Confirm-TrackedAbsent $Item)) { throw "Owned process survived cleanup: $($Item.ProcessId)" }
}
