    $managed = Join-Path $taskProfile "bin/runtime/$RuntimeVersion"
    if ($publishedOnline) {
        $installedCli = Join-Path $taskProfile 'bin/rsrs.exe'
        $report.online_install.cli_absent_before = -not (Test-Path -LiteralPath $installedCli)
        $report.online_install.runtime_absent_before = -not (Test-Path -LiteralPath (Join-Path $taskProfile 'bin/runtime.json'))
        if (-not $report.online_install.cli_absent_before -or -not $report.online_install.runtime_absent_before -or
            (Test-Path -LiteralPath (Join-Path $root 'bootstrap'))) { throw 'Online installation requires absent CLI, Runtime and bootstrap payload' }
        $install = Child-Start $exe @('--headless','install-runtime') -CaptureOutput
        $stdout = $install.StandardOutput.ReadToEndAsync(); $stderr = $install.StandardError.ReadToEndAsync()
        if (-not $install.WaitForExit(960000)) { $install.Kill($true); throw 'Client2 online installation exceeded its bounded deadline' }
        $installText = $stdout.GetAwaiter().GetResult()
        $installText | Set-Content -LiteralPath (Join-Path $root 'online-install.stdout.json') -Encoding utf8NoBOM
        $stderr.GetAwaiter().GetResult() | Set-Content -LiteralPath (Join-Path $root 'online-install.stderr.txt') -Encoding utf8NoBOM
        $report.online_install.exit_code = $install.ExitCode
        if ($install.ExitCode -ne 0 -or ($installText | ConvertFrom-Json).ok -ne $true) { throw 'Released Client2 online installation failed' }
        if (Test-Path -LiteralPath $installedCli) { throw 'Preparing Runtime unexpectedly installed the CLI' }
        $prepareCli = Child-Start $exe @('--headless','prepare-cli') -CaptureOutput
        $cliStdout = $prepareCli.StandardOutput.ReadToEndAsync(); $cliStderr = $prepareCli.StandardError.ReadToEndAsync()
        if (-not $prepareCli.WaitForExit(960000)) { $prepareCli.Kill($true); throw 'Client2 online CLI preparation exceeded its bounded deadline' }
        $cliText = $cliStdout.GetAwaiter().GetResult()
        $cliText | Set-Content -LiteralPath (Join-Path $root 'online-prepare-cli.stdout.json') -Encoding utf8NoBOM
        $cliStderr.GetAwaiter().GetResult() | Set-Content -LiteralPath (Join-Path $root 'online-prepare-cli.stderr.txt') -Encoding utf8NoBOM
        if ($prepareCli.ExitCode -ne 0 -or ($cliText | ConvertFrom-Json).ok -ne $true) { throw 'Released Client2 online CLI preparation failed' }
        $selection = Get-Content -Raw -LiteralPath (Join-Path $taskProfile 'bin/runtime.json') | ConvertFrom-Json
        if ($selection.version -cne $RuntimeVersion -or $selection.sha256 -cne $consumer.runtime_binary_sha256 -or
            (Get-FileHash -LiteralPath (Join-Path $managed 'rsrs-runtime.exe') -Algorithm SHA256).Hash.ToLowerInvariant() -cne $consumer.runtime_binary_sha256 -or
            (Get-FileHash -LiteralPath $installedCli -Algorithm SHA256).Hash.ToLowerInvariant() -cne $consumer.cli_binary_sha256 -or
            (Get-Item -LiteralPath $installedCli).Length -ne (Get-Item -LiteralPath $cliPath).Length) { throw 'Online installed CLI/Runtime differs from the exact published assets' }
        # The installer verifies the full archive, then removes release-only core-notices.
        foreach ($file in @($runtimeMetadata.files | Where-Object { -not $_.path.StartsWith('core-notices/', [StringComparison]::Ordinal) })) {
            $installedFile = Join-Path $managed $file.path
            if ((Get-Item -LiteralPath $installedFile).Length -ne $file.size -or
                (Get-FileHash -LiteralPath $installedFile -Algorithm SHA256).Hash.ToLowerInvariant() -cne $file.sha256) {
                throw 'Online installed Runtime payload differs from the published inventory'
            }
        }
        if (@(Get-ChildItem -LiteralPath (Join-Path $taskProfile 'client2') -Directory -Filter '.cli-download-*').Count) { throw 'CLI download staging survived installation' }
        $versionProcess = Child-Start $installedCli @('--version') -CaptureOutput
        $versionOut = $versionProcess.StandardOutput.ReadToEndAsync(); $versionErr = $versionProcess.StandardError.ReadToEndAsync()
        if (-not $versionProcess.WaitForExit(10000)) { $versionProcess.Kill($true); throw 'Installed CLI version check timed out' }
        $versionText = $versionOut.GetAwaiter().GetResult()
        $versionText | Set-Content -LiteralPath (Join-Path $root 'online-cli-version.stdout.txt') -Encoding utf8NoBOM
        $versionErr.GetAwaiter().GetResult() | Set-Content -LiteralPath (Join-Path $root 'online-cli-version.stderr.txt') -Encoding utf8NoBOM
        if ($versionProcess.ExitCode -ne 0 -or ($versionText.Trim() -split '\s+')[-1] -cne $published.cli.version) { throw 'Installed CLI version differs from the published reference' }
        foreach ($kind in @('cli','runtime')) {
            $releases = Invoke-RestMethod -Uri "https://api.github.com/repos/risense-ai/respire-$kind/releases?per_page=20" -TimeoutSec 60
            $releases | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $root "online-$kind-releases-after.json") -Encoding utf8NoBOM
        }
        $report.online_install.cli_path = $installedCli
        $report.online_install.cli_sha256 = $consumer.cli_binary_sha256
        $report.online_install.runtime_sha256 = $selection.sha256
        $report.online_install.completed = $true
    } else {
        $install = Child-Start $cliPath @('--json','runtime','install','--file',$archivePath,'--version',$RuntimeVersion)
        if (-not $install.WaitForExit(60000)) { $install.Kill($true); throw 'Staged Runtime installation timed out' }
        if ($install.ExitCode -ne 0) { throw 'Staged Runtime installation failed' }
        $selection = Get-Content -Raw -LiteralPath (Join-Path $taskProfile 'bin/runtime.json') | ConvertFrom-Json
        if ($candidateAssets) {
            if ($selection.version -cne $RuntimeVersion -or $selection.sha256 -cne $consumer.runtime_binary_sha256 -or
                (Get-FileHash -LiteralPath (Join-Path $managed 'rsrs-runtime.exe') -Algorithm SHA256).Hash.ToLowerInvariant() -cne $consumer.runtime_binary_sha256) { throw 'Installed candidate Runtime image differs' }
            # The installer verifies the full archive, then removes release-only core-notices.
            foreach ($file in @($runtimeMetadata.files | Where-Object { -not $_.path.StartsWith('core-notices/', [StringComparison]::Ordinal) })) {
                $name = if ($file.path -ceq $runtimeMetadata.binary_file) { 'rsrs-runtime.exe' } else { $file.path }
                $installedFile = Join-Path $managed $name
                if ((Get-Item -LiteralPath $installedFile).Length -ne $file.size -or
                    (Get-FileHash -LiteralPath $installedFile -Algorithm SHA256).Hash.ToLowerInvariant() -cne $file.sha256) { throw 'Installed candidate Runtime payload differs' }
            }
        }
        Copy-Item -LiteralPath $cliPath -Destination (Join-Path $taskProfile 'bin/rsrs.exe')
    }
    Write-Output 'GUI probe: Runtime installed'
    if ($CoreConfigQuery) {
        if ($selection.sha256 -cne $queryInputs.runtime_binary_sha256 -or
            (Get-FileHash -LiteralPath (Join-Path $managed 'rsrs-runtime.exe') -Algorithm SHA256).Hash.ToLowerInvariant() -cne $queryInputs.runtime_binary_sha256) { throw 'Real installed Runtime image differs' }
        # Fresh profile: no preparation setter. The native helper verifies the
        # initial selection before issuing the single live GUI save.
        $startRuntime = Child-Start (Join-Path $taskProfile 'bin/rsrs.exe') @('--json','runtime','start') -CaptureOutput
        $startOut = $startRuntime.StandardOutput.ReadToEndAsync(); $startErr = $startRuntime.StandardError.ReadToEndAsync()
        if (-not $startRuntime.WaitForExit(60000)) { $startRuntime.Kill(); $startRuntime.WaitForExit(); throw 'Real Runtime startup timed out' }
        $startOut.GetAwaiter().GetResult() | Out-Null; $startErr.GetAwaiter().GetResult() | Out-Null
        if ($startRuntime.ExitCode -ne 0) { throw 'Real Runtime startup failed' }
        $directHealth = Invoke-RestMethod -Uri "http://127.0.0.1:$rpcPort/api/health" -Headers @{'X-Respire-Health'='runtime-v1'} -TimeoutSec 10
        $coreRuntimeRecord = Get-CimInstance Win32_Process -Filter "ProcessId=$($directHealth.pid)"
        if (-not $coreRuntimeRecord -or -not [string]::Equals($coreRuntimeRecord.ExecutablePath,(Join-Path $managed 'rsrs-runtime.exe'),[StringComparison]::OrdinalIgnoreCase)) { throw 'Cannot bind real Runtime cleanup actor' }
        $directPort = $rpcPort
        $readyPath = Join-Path $root 'core-query-proxy-ready.json'
        $python = (Get-Command python -CommandType Application | Select-Object -First 1).Source
        $coreProxy = Child-Start $python @((Join-Path $PSScriptRoot 'dev-core-config-query-fixture.py'),'serve','--port',"$rpcPort",'--profile',$taskProfile,
            '--image',(Join-Path $managed 'rsrs-runtime.exe'),'--image-sha256',$queryInputs.runtime_binary_sha256,'--ready',$readyPath)
        $clock = [Diagnostics.Stopwatch]::StartNew()
        while (-not (Test-Path -LiteralPath $readyPath) -and $clock.Elapsed.TotalSeconds -lt 15 -and -not $coreProxy.HasExited) { Start-Sleep -Milliseconds 100 }
        $queryProxy = Get-Content -Raw -LiteralPath $readyPath | ConvertFrom-Json
        $rpcPort = ([uri]$queryProxy.address).Port
        $childEnv['RSRS_RPC_PORT'] = "$rpcPort"
    }
    if ($candidateProof -and (Get-FileHash -LiteralPath (Join-Path $managed 'rsrs-runtime.exe') -Algorithm SHA256).Hash.ToLowerInvariant() -ne $candidateProof.binary_sha256) {
        throw 'Installed candidate binary differs from the frozen original archive'
    }
    if ($OfflineAcceptance) {
        $modelRoot = [IO.Path]::GetFullPath($OfflineModelDirectory)
        if (-not $modelRoot.StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase)) { throw 'Offline models must be staged under RUNNER_TEMP' }
        $modelHashes = @{
            'tokenizer.json'='6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790'
            'onnx/model_quantized.onnx'='0826f8c1ab9edf1801db86c61919d4d108e8bfc0b809ec823ad366882ff0b77d'
            'onnx/model_quantized.cpu-v1.ort'='7ff152e912bc234f5c90e4012f99897a7b2ce180f016db2321304057756b64ff'
        }
        foreach ($name in $modelHashes.Keys) {
            $modelFile = Join-Path $modelRoot $name
            foreach ($checkPath in @($modelRoot,(Split-Path $modelFile),$modelFile)) {
                if ((Get-Item -LiteralPath $checkPath).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Offline model input may not be linked' }
            }
            if ((Get-FileHash -LiteralPath $modelFile -Algorithm SHA256).Hash.ToLowerInvariant() -ne $modelHashes[$name]) { throw 'Pinned offline CPU model bytes differ' }
        }
        $report.offline_model_hashes = $modelHashes
        $childEnv['RSRS_M3_DIR'] = $modelRoot; $childEnv['RSRS_ENGINE'] = 'cpu'
    }
    $phases = if ($fullFixture -and $OfflineAcceptance) { @('initial','restart','recovery-issue','recovery-resume','recovery-verified') } else { @('initial') }
    if ($CoreConfigQuery) { $phases = @('core-issue','core-resume') }
