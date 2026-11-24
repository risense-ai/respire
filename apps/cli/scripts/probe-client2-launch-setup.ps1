    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class RespireGuiNative {
    public delegate bool EnumProc(IntPtr hwnd, IntPtr arg);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr arg);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr hwnd, out RECT rect);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attribute, out int value, int size);
    [DllImport("user32.dll", SetLastError=true)] static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
    [DllImport("user32.dll")] static extern bool CloseDesktop(IntPtr desktop);
    [StructLayout(LayoutKind.Sequential)] struct RECT { public int Left, Top, Right, Bottom; }
    public static bool InputDesktopAvailable() {
        var handle = OpenInputDesktop(0, false, 1);
        if (handle == IntPtr.Zero) return false;
        CloseDesktop(handle); return true;
    }
    public static object[] Windows(uint processId) {
        var rows = new List<object>();
        EnumWindows((hwnd, arg) => {
            uint owner; GetWindowThreadProcessId(hwnd, out owner);
            if (owner != processId) return true;
            RECT rect; bool rectOk = GetClientRect(hwnd, out rect);
            int cloaked; int hr = DwmGetWindowAttribute(hwnd, 14, out cloaked, sizeof(int));
            rows.Add(new { hwnd = hwnd.ToInt64(), visible = IsWindowVisible(hwnd),
                width = rect.Right - rect.Left, height = rect.Bottom - rect.Top,
                rect_ok = rectOk, cloak_query_ok = hr == 0, cloaked = cloaked });
            return true;
        }, IntPtr.Zero);
        return rows.ToArray();
    }
}
'@
    $report.environment = [ordered]@{
        image_os = $env:ImageOS; image_version = $env:ImageVersion
        os_version = [Environment]::OSVersion.VersionString
        session_id = (Get-Process -Id $PID).SessionId
        user_interactive = [Environment]::UserInteractive
        input_desktop_available = [RespireGuiNative]::InputDesktopAvailable()
    }
    $webviewRoots = @(${env:ProgramFiles(x86)}, $env:ProgramFiles) | Where-Object { $_ }
    $webview = @($webviewRoots | ForEach-Object {
        Get-ChildItem -Path (Join-Path $_ 'Microsoft/EdgeWebView/Application/*/msedgewebview2.exe') -ErrorAction SilentlyContinue
    })
    $report.environment.webview2_installed = $webview.Count -gt 0
    $report.environment.webview2_versions = @($webview | ForEach-Object { $_.VersionInfo.FileVersion })
    if (-not $report.environment.user_interactive -or -not $report.environment.input_desktop_available -or -not $webview.Count) {
        $report.status = 'environment-blocked'
        throw 'Hosted image lacks an interactive input desktop or discoverable WebView2 Runtime; no installation attempted'
    }
    $report.environment_ready = $true
    $exe = Join-Path $root 'respire-windows-amd64.exe'
    if ($CoreConfigQuery) {
        if ($OfflineAcceptance -or $publishedAssets -or $candidateAssets -or $RuntimeVersion -cne '2.0.0-dev.24') { throw 'Core query is a separate real Runtime24 diagnostic' }
        $queryInputs = Get-Content -Raw -LiteralPath $CoreQueryInputs | ConvertFrom-Json
        $sealedManifestPath = Join-Path $SealedClient2Directory 'candidate.json'
        $sealed = Get-Content -Raw -LiteralPath $sealedManifestPath | ConvertFrom-Json
        $sourceExe = Join-Path $SealedClient2Directory 'respire-windows-amd64.exe'
        if ($queryInputs.schema -ne 1 -or $queryInputs.mode -cne 'core-query-diagnostic-inputs' -or
            $queryInputs.harness_sha -cne $env:GITHUB_SHA -or $queryInputs.client2_source -cne $Client2Commit -or
            $sealed.source -cne $Client2Commit -or $sealed.version -cne '2.0.0-dev.17' -or
            (Get-FileHash -LiteralPath $sealedManifestPath -Algorithm SHA256).Hash.ToLowerInvariant() -cne $queryInputs.client2_manifest_sha256 -or
            (Get-FileHash -LiteralPath $sourceExe -Algorithm SHA256).Hash.ToLowerInvariant() -cne $sealed.files.'respire-windows-amd64.exe'.sha256) { throw 'Sealed desktop/input binding differs' }
        Copy-Item -LiteralPath $sourceExe -Destination $exe
        if ((Get-Item -LiteralPath $exe).Length -ne $sealed.files.'respire-windows-amd64.exe'.size -or
            (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant() -cne $sealed.files.'respire-windows-amd64.exe'.sha256) { throw 'Copied sealed executable differs' }
        $report.scope = 'real WebView core-config original-query diagnostic only'
        $report.diagnostic_only = $true; $report.core_test_mode = $false
        $report.exe_sha256 = $sealed.files.'respire-windows-amd64.exe'.sha256
        $report.core_query_inputs = $queryInputs
    } elseif ($SourceCandidate) {
        if (-not $OfflineAcceptance) { throw 'Source candidate requires the complete offline GUI subset' }
        $manifest=Copy-SourceClient2Candidate $SourceClient2Exe $SourceClient2Manifest $SourceClient2Commit $exe
        $report.scope='source Client2 Windows GUI subset; not official asset or release acceptance'
        foreach ($key in @('release','asset_id','source_sha','expected_exe_sha256')) { $report.Remove($key) }
        $report.fixture_mode='source-client2-ui-candidate'
        $report.source_candidate=$manifest
        $report.source_candidate_verified=$true
        $report.exe_sha256=$manifest.exe_sha256
        $report.unverified_ui_scenarios=@('pending','error','cancel')
    } elseif ($candidate.client2.sealed) {
        Save-SealedClient2 $exe
    } else {
        if ($client2Candidate -and -not $OfflineAcceptance) { throw 'Desktop candidate requires the complete offline GUI subset' }
        Save-DownloadedClient2 $exe
    }
    $cliPath = (Resolve-Path -LiteralPath $Cli).Path
    $archivePath = (Resolve-Path -LiteralPath $RuntimeArchive).Path
    $report.cli_sha256 = (Get-FileHash -LiteralPath $cliPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $report.runtime_archive_sha256 = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($CoreConfigQuery -and ($report.cli_sha256 -cne $queryInputs.cli_sha256 -or
        $report.runtime_archive_sha256 -cne $queryInputs.runtime_archive_sha256)) { throw 'Verified CLI/Runtime input bytes differ' }
    $candidateProofPath = Join-Path (Split-Path $archivePath) 'candidate-source.json'
    if (Test-Path -LiteralPath $candidateProofPath) {
        if ((Get-Item -LiteralPath $candidateProofPath).Length -gt 65536) { throw 'Candidate provenance exceeds its bound' }
        $candidateProof = Get-Content -Raw -LiteralPath $candidateProofPath | ConvertFrom-Json
        if ($candidateProof.mode -ne 'frozen-runtime-candidate' -or $candidateProof.target -ne 'x86_64-pc-windows-msvc' -or
            $candidateProof.runtime_version -ne $RuntimeVersion -or $candidateProof.archive_sha256 -ne $report.runtime_archive_sha256 -or
            $candidateProof.actual_native_sdk_archive -ne 'not-evaluated' -or $candidateProof.full_gui_acceptance -ne 'not-evaluated' -or
            $candidateProof.g1_passed -ne $false) { throw 'Candidate archive/provenance binding differs' }
        $report.runtime_candidate = $candidateProof
    }
    if ($candidateAssets) {
        $manifestHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($env:CANDIDATE_ASSETS))).ToLowerInvariant()
        $consumer = Get-Content -Raw -LiteralPath (Join-Path (Split-Path $cliPath) 'candidate-consumer-identity.json') | ConvertFrom-Json
        $metadataPath = (Resolve-Path -LiteralPath $env:RSRS_PUBLISHED_RUNTIME_METADATA).Path
        $runtimeMetadata = Get-Content -Raw -LiteralPath $metadataPath | ConvertFrom-Json
        if ($candidate.kind -cne 'unpublished-cli-candidate' -or $candidate.source.sha -cne $env:GITHUB_SHA -or
            $candidate.source.run_id -ne [long]$env:GITHUB_RUN_ID -or $candidate.source.run_attempt -ne [int]$env:GITHUB_RUN_ATTEMPT -or
            $consumer.manifest_sha256 -cne $manifestHash -or $consumer.target -cne 'x86_64-pc-windows-msvc' -or
            $consumer.harness_sha -cne $env:GITHUB_SHA -or $consumer.product_sources.cli -cne $candidate.source.sha -or
            $consumer.product_sources.runtime -cne $candidate.runtime.source_sha -or $consumer.cli_binary_sha256 -cne $report.cli_sha256 -or
            $consumer.runtime_bundle_sha256 -cne $report.runtime_archive_sha256 -or $candidate.runtime.version -cne $RuntimeVersion -or
            $runtimeMetadata.runtime_version -cne $RuntimeVersion -or $runtimeMetadata.target_triple -cne 'x86_64-pc-windows-msvc' -or $runtimeMetadata.source_commit -cne $candidate.runtime.source_sha -or
            $runtimeMetadata.binary_sha256 -cne $consumer.runtime_binary_sha256 -or $runtimeMetadata.archive_sha256 -cne $report.runtime_archive_sha256 -or
            (Get-FileHash -LiteralPath $metadataPath -Algorithm SHA256).Hash.ToLowerInvariant() -cne $env:RSRS_PUBLISHED_RUNTIME_METADATA_SHA256) {
            throw 'Candidate CLI/Runtime reference identity differs from verified consumer receipt'
        }
        $report.candidate_inputs = @{manifest_sha256=$manifestHash; cli_source=$candidate.source.sha; runtime_source=$candidate.runtime.source_sha}
        $report.online_install = @{mode='not-evaluated'; completed=$false; reason='unpublished CLI candidate; exact local asset installation'}
    }
    if ($RuntimeVersion -notmatch '^\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?$') { throw 'Invalid pinned Runtime version' }
    $taskHome = Join-Path $root 'profile/home'
    $taskProfile = if ($publishedOnline) { Join-Path $taskHome '.rsrs' } else { Join-Path $root 'profile' }
    if ($publishedAssets) {
        if ($candidateProof) { throw 'Published acceptance cannot consume a Runtime candidate' }
        $manifestHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($env:PUBLISHED_ASSETS))).ToLowerInvariant()
        $published = $env:PUBLISHED_ASSETS | ConvertFrom-Json
        $consumer = Get-Content -Raw -LiteralPath (Join-Path (Split-Path $cliPath) 'published-consumer-identity.json') | ConvertFrom-Json
        $metadataPath = (Resolve-Path -LiteralPath $env:RSRS_PUBLISHED_RUNTIME_METADATA).Path
        $runtimeMetadata = Get-Content -Raw -LiteralPath $metadataPath | ConvertFrom-Json
        if ($manifestHash -cne $env:PUBLISHED_ASSETS_SHA256 -or $consumer.manifest_sha256 -cne $manifestHash -or
            $consumer.target -cne 'x86_64-pc-windows-msvc' -or $consumer.harness_sha -cne $env:GITHUB_SHA -or
            $consumer.product_sources.cli -cne $published.cli.source_sha -or $consumer.product_sources.runtime -cne $published.runtime.source_sha -or
            $consumer.cli_binary_sha256 -cne $report.cli_sha256 -or $consumer.runtime_bundle_sha256 -cne $report.runtime_archive_sha256 -or
            $published.runtime.version -cne $RuntimeVersion -or $runtimeMetadata.runtime_version -cne $RuntimeVersion -or
            $runtimeMetadata.source_commit -cne $published.runtime.source_sha -or $runtimeMetadata.binary_sha256 -cne $consumer.runtime_binary_sha256 -or
            $runtimeMetadata.archive_sha256 -cne $report.runtime_archive_sha256 -or
            (Get-FileHash -LiteralPath $metadataPath -Algorithm SHA256).Hash.ToLowerInvariant() -cne $env:RSRS_PUBLISHED_RUNTIME_METADATA_SHA256) {
            throw 'Published CLI/Runtime reference identity differs from the verified consumer receipt'
        }
        $report.published_inputs = @{manifest_sha256=$manifestHash; cli_source=$published.cli.source_sha; runtime_source=$published.runtime.source_sha}
        if ($publishedOnline) {
            $report.online_install = @{mode='published-assets'; manifest_sha256=$manifestHash; cli_version=$published.cli.version
                cli_source=$published.cli.source_sha; runtime_version=$RuntimeVersion; runtime_source=$published.runtime.source_sha
                home=$taskHome; data_root=$taskProfile; channel_source='product-default'; completed=$false}
            foreach ($kind in @('cli','runtime')) {
                $releases = Invoke-RestMethod -Uri "https://api.github.com/repos/risense-ai/respire-$kind/releases?per_page=20" -TimeoutSec 60
                $releases | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $root "online-$kind-releases-before.json") -Encoding utf8NoBOM
                $latest = $releases | Where-Object { -not $_.draft -and $_.tag_name -cmatch '^v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$' } |
                    Sort-Object -Property @{Expression={[semver]$_.tag_name.Substring(1)}; Descending=$true} | Select-Object -First 1
                if ($latest.tag_name -cne ('v'+$published.$kind.version) -or $latest.id -ne $published.$kind.release_id) {
                    throw "Dev discovery does not select the exact published $kind reference; no product installation attempted"
                }
            }
        } else {
            $report.online_install = @{mode='not-evaluated'; completed=$false; reason='authenticated Client2 CI candidate; not public product upgrade evidence'}
        }
    }
    foreach ($dir in @('home','appdata','local','temp','bin')) { New-Item -ItemType Directory -Path (Join-Path $taskProfile $dir) | Out-Null }
    # Original Client2 reads these exact keys; RSRS_UPDATE_CHECK does not control its background loop.
    New-Item -ItemType Directory -Path (Join-Path $taskProfile 'client2') | Out-Null
    $preferencesJson = @'
{
  "autoRuntime": false,
  "autoUpdate": false,
  "clientAutoUpdate": false,
  "clientAutoRestart": false,
  "runtimeAutoUpdate": false,
  "runtimeAutoRestart": false,
  "onboardingVersion": 1,
  "onboardingCompleted": false,
  "onboardingStep": "language",
  "onboardingSyncChoice": ""
}
'@
    $preferencesPath = Join-Path $taskProfile 'client2/preferences.json'
    $preferencesJson | Set-Content -LiteralPath $preferencesPath -Encoding utf8NoBOM
    if ($CoreConfigQuery) {
        $queryPreferences = Get-Content -Raw -LiteralPath $preferencesPath | ConvertFrom-Json
        $queryPreferences.onboardingCompleted = $true
        $queryPreferences | Add-Member -NotePropertyName language -NotePropertyValue 'zh'
        $queryPreferences | ConvertTo-Json | Set-Content -LiteralPath $preferencesPath -Encoding utf8NoBOM
    }
    $report.original_client_preferences = Get-Content -Raw -LiteralPath $preferencesPath | ConvertFrom-Json
    function Free-Port {
        $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
        $listener.Start(); $number = $listener.LocalEndpoint.Port; $listener.Stop(); return $number
    }
    $rpcPort = Free-Port
    # Explicit environment allowlist: never copy GitHub/download/runtime-service tokens into children.
    $childEnv = @{}
    foreach ($key in @('SystemRoot','WINDIR','COMSPEC','PATHEXT','ProgramFiles','ProgramFiles(x86)','ProgramData')) {
        $value = [Environment]::GetEnvironmentVariable($key); if ($value) { $childEnv[$key] = $value }
    }
    $childEnv['PATH'] = "$($childEnv['SystemRoot'])\System32;$($childEnv['SystemRoot'])"
    $childEnv['HOME'] = $taskHome; $childEnv['USERPROFILE'] = $childEnv['HOME']
    $childEnv['APPDATA'] = Join-Path $taskProfile 'appdata'; $childEnv['LOCALAPPDATA'] = Join-Path $taskProfile 'local'
    $childEnv['TEMP'] = Join-Path $taskProfile 'temp'; $childEnv['TMP'] = $childEnv['TEMP']
    $childEnv['RSRS_DATA_DIR'] = $taskProfile; $childEnv['RSRS_BIN_DIR'] = Join-Path $taskProfile 'bin'
    $childEnv['RSRS_RPC_PORT'] = "$rpcPort"; $childEnv['RSRS_INSTANCE_ID'] = [Guid]::NewGuid().ToString('N')
    $childEnv['RSRS_CORE_TEST_MODE'] = '1'; $childEnv['RSRS_NO_AUTOSYNC'] = '1'; $childEnv['RSRS_UPDATE_CHECK'] = '0'
    if ($CoreConfigQuery) { $childEnv.Remove('RSRS_CORE_TEST_MODE') }
    function Child-Start([string]$file, [string[]]$argv, [switch]$CaptureOutput, [switch]$CaptureInput) {
        $start = [Diagnostics.ProcessStartInfo]::new($file)
        $start.UseShellExecute = $false; $start.WorkingDirectory = $root
        $start.RedirectStandardOutput = [bool]$CaptureOutput; $start.RedirectStandardError = [bool]$CaptureOutput
        $start.RedirectStandardInput = [bool]$CaptureInput
        $start.Environment.Clear()
        foreach ($key in $childEnv.Keys) { $start.Environment[$key] = $childEnv[$key] }
        foreach ($arg in $argv) { [void]$start.ArgumentList.Add($arg) }
        return [Diagnostics.Process]::Start($start)
    }
