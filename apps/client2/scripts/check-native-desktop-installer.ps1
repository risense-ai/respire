param(
    [Parameter(Mandatory)][string]$Candidate,
    [Parameter(Mandatory)][string]$Work,
    [Parameter(Mandatory)][ValidateSet('amd64', 'arm64')][string]$Arch,
    [Parameter(Mandatory)][string]$Version
)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_ENVIRONMENT -cne 'github-hosted' -or
    $env:GITHUB_REPOSITORY -cne 'risense-ai/respire' -or $env:RUNNER_OS -cne 'Windows') {
    throw 'Windows installer check requires a disposable GitHub-hosted runner'
}
$expectedArch = if ($Arch -eq 'amd64') { 'X64' } else { 'ARM64' }
if ($env:RUNNER_ARCH -cne $expectedArch -or
    [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString().ToUpperInvariant() -cne $expectedArch -or
    $env:CANDIDATE_VERSION -cne $Version) {
    throw 'Windows installer runner identity differs'
}
$tempRoot = (Resolve-Path -LiteralPath $env:RUNNER_TEMP).Path.TrimEnd('\') + '\'
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
$Work = (Resolve-Path -LiteralPath $Work).Path
if (!$Candidate.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
    !$Work.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
    (Split-Path -Leaf $Work) -notlike 'respire-desktop-check-*') { throw 'Unsafe installer check paths' }
$appRoot = Join-Path $Work 'installer-app'
$setup = Join-Path $Candidate "respire-windows-$Arch-setup.exe"
$setupInput = Join-Path $Candidate "respire-windows-$Arch.exe"
$installerRegistry = 'HKCU:\Software\Respire\Installer'
$uninstallKeys = @(
    'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{AFCA612B-7DD5-4380-8DD2-ADE8E2DA28C4}_is1',
    'HKCU:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\{AFCA612B-7DD5-4380-8DD2-ADE8E2DA28C4}_is1'
)
if (($uninstallKeys | Where-Object { Test-Path -LiteralPath $_ }) -or
    (Test-Path -LiteralPath $installerRegistry) -or (Test-Path -LiteralPath $appRoot)) {
    throw 'Existing Respire installation or installer state'
}
$webViewKey = 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
$webViewVersions = @('HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
    "HKLM:\$webViewKey", "HKCU:\$webViewKey") | ForEach-Object {
        (Get-ItemProperty -LiteralPath $_ -Name pv -ErrorAction SilentlyContinue).pv
    }
if (!($webViewVersions | Where-Object { $_ -and ([version]$_ -ge [version]'86.0.622.0') })) {
    throw 'Preinstalled WebView2 required: do not let the package bootstrap external dependencies'
}
$env:RSRS_DATA_DIR = Join-Path $Work 'existing-data'
New-Item -ItemType Directory -Path (Join-Path $env:RSRS_DATA_DIR 'client2') | Out-Null
$preferences = Join-Path $env:RSRS_DATA_DIR 'client2\preferences.json'
$sentinel = Join-Path $env:RSRS_DATA_DIR 'legacy-data.sentinel'
[IO.File]::WriteAllText($preferences, '{"theme":"dark","language":"en"}')
[IO.File]::WriteAllText($sentinel, 'preserve-existing-data')
function FileDigest([string]$InputFile) { (Get-FileHash -LiteralPath $InputFile -Algorithm SHA256).Hash }
$preferencesHash = FileDigest $preferences
$sentinelHash = FileDigest $sentinel
$pathBefore = (Get-ItemProperty -LiteralPath 'HKCU:\Environment' -Name Path -ErrorAction SilentlyContinue).Path
function TestProcess([string]$FilePath, [string[]]$Arguments, [int]$Milliseconds, [string]$Output = '') {
    $options = @{ FilePath = $FilePath; ArgumentList = $Arguments; WindowStyle = 'Hidden'; PassThru = $true }
    if ($Output) { $options.RedirectStandardOutput = $Output }
    $process = Start-Process @options
    try {
        if (!$process.WaitForExit($Milliseconds)) {
            try { $process.Kill($true) } finally { $process.WaitForExit() }
            throw 'Native installer check timed out'
        }
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) { throw "Native check exited $($process.ExitCode)" }
    } finally { $process.Dispose() }
}
$installedBinary = Join-Path $appRoot 'Respire.exe'
$uninstaller = Join-Path $appRoot 'unins000.exe'
try {
    $installLog = Join-Path $Work 'install.log'
    TestProcess -FilePath $setup -Arguments @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-',
        '/NOICONS', '/TASKS=""', ('/DIR="{0}"' -f $appRoot), ('/LOG="{0}"' -f $installLog)) -Milliseconds 120000
    if ((Test-Path -LiteralPath (Join-Path $appRoot 'bootstrap')) -or
        (FileDigest $installedBinary) -cne (FileDigest $setupInput)) { throw 'Installed product bytes differ' }
    $versionOutput = Join-Path $Work 'installed-version.json'
    TestProcess -FilePath $installedBinary -Arguments @('--version') -Milliseconds 15000 -Output $versionOutput
    $info = Get-Content -LiteralPath $versionOutput -Raw | ConvertFrom-Json
    if ($info.version -cne $Version -or $info.platform -cne "windows-$Arch") { throw 'Installed product identity differs' }
    $fontsOutput = Join-Path $Work 'installed-fonts.json'
    TestProcess -FilePath $installedBinary -Arguments @('--headless', 'fonts') -Milliseconds 15000 -Output $fontsOutput
    $fonts = Get-Content -LiteralPath $fontsOutput -Raw | ConvertFrom-Json
    if (!$fonts.ok -or $fonts.result.Count -eq 0) { throw 'Installed product did not enumerate native fonts' }
} finally {
    if (Test-Path -LiteralPath $uninstaller -PathType Leaf) {
        TestProcess -FilePath $uninstaller -Arguments @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') -Milliseconds 60000
    }
}
if ((Test-Path -LiteralPath $installedBinary) -or ($uninstallKeys | Where-Object { Test-Path -LiteralPath $_ }) -or
    (Test-Path -LiteralPath $installerRegistry) -or
    (FileDigest $preferences) -cne $preferencesHash -or (FileDigest $sentinel) -cne $sentinelHash -or
    (Get-ItemProperty -LiteralPath 'HKCU:\Environment' -Name Path -ErrorAction SilentlyContinue).Path -cne $pathBefore) {
    throw 'Install/uninstall changed existing data, PATH or installation registration'
}
