$runtimeStatus = Invoke-RuntimeCli 'runtime-cli-status' @('status')
Assert-Smoke 'runtime-cli-status-approved-server' ([string]$runtimeStatus.summary.server_addr -ceq $Server -and $runtimeStatus.summary.unlocked -eq $true)
Assert-Smoke 'runtime-cli-workspace-mode-normal' ([string]$runtimeStatus.summary.workspace -ceq 'normal')
foreach ($hostOnly in @(@('config'), @('agent-config'), @('inject', '--targets'))) {
    $deniedName = "runtime-cli-$($hostOnly[0])-host-boundary"
    $denied = Invoke-RuntimeCli $deniedName $hostOnly -ExpectedExit 2
    if ($hostOnly[0] -ceq 'inject') {
        # Runtime23 removes inject from its parser; it cannot reach host-command admission.
        Assert-Smoke "$deniedName-fenced" ([string]$denied.command -ceq 'cli' `
            -and [string]$denied.summary.reason -ceq 'invalid_input' `
            -and [string]$denied.details.error_type -ceq 'user' `
            -and @($denied.errors).Count -eq 1 -and [string]$denied.errors[0] -ceq "error: unrecognized subcommand 'inject'")
    } else {
        Assert-Smoke "$deniedName-fenced" ([string]$denied.details.error_type -ceq 'host_boundary')
    }
}
$hostConfig = Invoke-Om -Name 'runtime-cli-config' -ArgList @('--json', 'config') -DataDir $DirA -TimeoutSec 90
$runtimeConfig = $hostConfig.Envelope
Assert-Smoke 'runtime-cli-config-response' ($hostConfig.Ok -and $hostConfig.Exit -eq 0 -and [string]$runtimeConfig.command -ceq 'config')
Assert-Smoke 'runtime-cli-config-approved-server' ([string]$runtimeConfig.summary.addr -ceq $Server -and $runtimeConfig.summary.autosync -eq $false)
$hostAgent = Invoke-Om -Name 'runtime-cli-agent-config' -ArgList @('--json', 'agent-config') -DataDir $DirA -TimeoutSec 90
$runtimeAgent = $hostAgent.Envelope
Assert-Smoke 'runtime-cli-agent-config-response' ($hostAgent.Ok -and $hostAgent.Exit -eq 0 -and [string]$runtimeAgent.command -ceq 'agent-config')
Assert-Smoke 'runtime-cli-diary-mode-contract' ([string]$runtimeAgent.summary.diary_mode -in @('concise', 'verbose'))
Invoke-RuntimeCli 'runtime-cli-list' @('list', '--limit', '5') | Out-Null
Invoke-RuntimeCli 'runtime-cli-tree' @('tree', '--depth', '3') | Out-Null
$hostTargets = Invoke-Om -Name 'runtime-cli-inject-targets' -ArgList @('--json', 'inject', '--targets') -DataDir $DirA -TimeoutSec 90
Assert-Smoke 'runtime-cli-inject-targets-response' ($hostTargets.Ok -and $hostTargets.Exit -eq 0 -and [string]$hostTargets.Envelope.command -ceq 'inject')
Assert-Smoke 'runtime-cli-retired-rerank-rejected' ($retiredInstall.Ok -and -not (Test-Path -LiteralPath (Join-Path $Root 'models/bge-reranker-base')))
$runtimeCreated = Invoke-RuntimeCli 'runtime-cli-create' @('remember', "runtime-rpc-$stamp", '--title', "runtime-$stamp", '--importance', 'important', '--force', '--parent', $id)
$runtimeId = [string]$runtimeCreated.summary.id
Assert-Smoke 'runtime-cli-created-id' ([bool]$runtimeId)
Invoke-RuntimeCli 'runtime-cli-update' @('update', $runtimeId, '--content', "runtime-updated-$stamp", '--title', "runtime-updated-$stamp") | Out-Null
$runtimeRead = Invoke-RuntimeCli 'runtime-cli-show' @('show', $runtimeId)
Assert-Smoke 'runtime-cli-update-persisted' ([string]$runtimeRead.details.entry.content -ceq "runtime-updated-$stamp")
Invoke-RuntimeCli 'runtime-cli-attach' @('attach', $runtimeId, '--parent', $splitId) | Out-Null
$runtimeParent = Invoke-RuntimeCli 'runtime-cli-parent-readback' @('show', $runtimeId)
Assert-Smoke 'runtime-cli-parent-persisted' ([string]$runtimeParent.details.entry.parent_id -ceq $splitId)
Invoke-RuntimeCli 'runtime-cli-delete' @('forget', $runtimeId) | Out-Null
Invoke-RuntimeCli 'runtime-cli-restore' @('restore', $runtimeId) | Out-Null
$runtimeRestored = Invoke-RuntimeCli 'runtime-cli-restore-readback' @('show', $runtimeId)
Assert-Smoke 'runtime-cli-restore-preserved-content' ([string]$runtimeRestored.details.entry.content -ceq "runtime-updated-$stamp")
Invoke-RuntimeCli 'runtime-cli-purge' @('purge', $runtimeId) | Out-Null
$wantTools = @(
    'memory_status','memory_extract','memory_remember','memory_recall','memory_list','memory_show','memory_update',
    'memory_attach','memory_tree','memory_history','memory_diary','memory_chain','memory_query_log_mark',
    'memory_taxonomy','memory_diary_search','memory_diary_fragment'
)
if ($mcpUrl) {
    try {
        $sse = Invoke-WebRequest -Uri $mcpSse -Headers $mcpHeaders -UseBasicParsing -TimeoutSec 15
        $sseText = [string]$sse.Content
        Add-SweepRow 'mcp-http-sse' (($sse.StatusCode -eq 200) -and ($sseText -like '*event: endpoint*') -and ($sseText -like '*/mcp*')) $mcpSse
    } catch {
        Add-SweepRow 'mcp-http-sse' $false $_.Exception.Message
    }
    function Invoke-McpHttpRpc($Name, $Payload) {
        try {
            $body = $Payload | ConvertTo-Json -Compress -Depth 12
            $resp = Invoke-WebRequest -Uri $mcpUrl -Method POST -Headers $mcpHeaders -ContentType 'application/json; charset=utf-8' -Body $body -UseBasicParsing -TimeoutSec 90
            $parsed = $resp.Content | ConvertFrom-Json
            Add-SweepRow $Name ($resp.StatusCode -eq 200 -and $null -ne $parsed -and [string]$parsed.jsonrpc -eq '2.0' -and [string]$parsed.id -eq [string]$Payload.id) $mcpUrl
            return $parsed
        } catch {
            Add-SweepRow $Name $false $_.Exception.Message
            return $null
        }
    }
    $httpInit = Invoke-McpHttpRpc 'mcp-http-initialize' @{ jsonrpc='2.0'; id=1; method='initialize'; params=@{ protocolVersion='2025-03-26'; capabilities=@{}; clientInfo=@{ name='sweep'; version='0' } } }
    Add-SweepRow 'mcp-http-initialize-version' ($null -ne $httpInit -and [string]$httpInit.result.protocolVersion -eq '2025-03-26') ([string]$httpInit.result.protocolVersion)
    $httpList = Invoke-McpHttpRpc 'mcp-http-tools-list' @{ jsonrpc='2.0'; id=2; method='tools/list' }
    $httpNames = @()
    if ($httpList -and $httpList.result.tools) { $httpNames = @($httpList.result.tools | ForEach-Object { [string]$_.name }) }
    $missingHttp = @($wantTools | Where-Object { $httpNames -notcontains $_ })
    $unexpectedHttp = @($httpNames | Where-Object { $wantTools -cnotcontains $_ })
    Add-SweepRow 'mcp-http-tools-complete' ($missingHttp.Count -eq 0 -and $unexpectedHttp.Count -eq 0 -and $httpNames.Count -eq 16) ("have=$($httpNames.Count) missing=$($missingHttp -join ',')")
    Invoke-McpHttpRpc 'mcp-http-ping' @{ jsonrpc='2.0'; id=3; method='ping' } | Out-Null
}

$mcpStdinList = (@(
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"sweep","version":"0"}}}'
    '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
    '{"jsonrpc":"2.0","id":3,"method":"ping"}'
) -join "`n") + "`n"
$mcpListRun = Invoke-Om -Name 'mcp-stdio-initialize-list' -ArgList @('mcp') -DataDir $DirA -Raw -TimeoutSec 90 -StdinText $mcpStdinList -Note 'initialize + tools/list + ping'
$mcpInit = Get-JsonRpc $mcpListRun.Stdout 1
$mcpList = Get-JsonRpc $mcpListRun.Stdout 2
$mcpPing = Get-JsonRpc $mcpListRun.Stdout 3
Add-SweepRow 'mcp-stdio-initialize-version' ($null -ne $mcpInit -and [string]$mcpInit.result.protocolVersion -eq '2025-03-26') ([string]$mcpInit.result.protocolVersion)
$stdioNames = @()
if ($mcpList -and $mcpList.result.tools) { $stdioNames = @($mcpList.result.tools | ForEach-Object { [string]$_.name }) }
$missingStdio = @($wantTools | Where-Object { $stdioNames -notcontains $_ })
$unexpectedStdio = @($stdioNames | Where-Object { $wantTools -cnotcontains $_ })
Add-SweepRow 'mcp-stdio-tools-complete' ($missingStdio.Count -eq 0 -and $unexpectedStdio.Count -eq 0 -and $stdioNames.Count -eq 16) ("have=$($stdioNames.Count) missing=$($missingStdio -join ',')")
Add-SweepRow 'mcp-stdio-ping' ($null -ne $mcpPing -and $null -ne $mcpPing.result) 'ping'

$mcpMarker = "mcp-business-$stamp"
$statusTool = Invoke-McpStdioTool -Name 'mcp-stdio-status' -Tool 'memory_status' -DataDir $DirA
Add-SweepRow 'mcp-stdio-status-envelope' ($null -ne $statusTool.Envelope -and [string]$statusTool.Envelope.command -eq 'status') ([string]$statusTool.Envelope.command)
$taxTool = Invoke-McpStdioTool -Name 'mcp-stdio-taxonomy' -Tool 'memory_taxonomy' -DataDir $DirA
Add-SweepRow 'mcp-stdio-taxonomy-envelope' ($null -ne $taxTool.Envelope -and [string]$taxTool.Envelope.command -eq 'taxonomy') ([string]$taxTool.Envelope.command)
$rememberTool = Invoke-McpStdioTool -Name 'mcp-stdio-remember' -Tool 'memory_remember' -Arguments @{
    content = "$MARK_CAUSE Sweep the MCP surface. $MARK_ACTION Write marker $mcpMarker. $MARK_RESULT Used by the recall/show/update/attach/forget assertions."
    title = "mcp-sweep-marker"
    importance = 'important'
    parent = $TX_DEV
    force = $true
} -DataDir $DirA
$mcpId = ''
if ($rememberTool.Envelope -and $rememberTool.Envelope.summary.id) { $mcpId = [string]$rememberTool.Envelope.summary.id }
if (-not $mcpId -and $rememberTool.Envelope -and $rememberTool.Envelope.summary.entry_id) { $mcpId = [string]$rememberTool.Envelope.summary.entry_id }
if (-not $mcpId -and $rememberTool.Stdout -match '([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})') { $mcpId = $Matches[1] }
Add-SweepRow 'mcp-stdio-remember-id' ([bool]$mcpId) $mcpId
$recallTool = Invoke-McpStdioTool -Name 'mcp-stdio-recall' -Tool 'memory_recall' -Arguments @{ query = $mcpMarker; limit = 3 } -DataDir $DirA
Add-SweepRow 'mcp-stdio-recall-hit' ($recallTool.Run.Stdout -like "*$mcpMarker*") 'recall contains marker'
$listTool = Invoke-McpStdioTool -Name 'mcp-stdio-list' -Tool 'memory_list' -Arguments @{ limit = 5 } -DataDir $DirA
Add-SweepRow 'mcp-stdio-list-envelope' ($null -ne $listTool.Envelope -and [string]$listTool.Envelope.command -eq 'list') ([string]$listTool.Envelope.command)
if ($mcpId) {
    $showTool = Invoke-McpStdioTool -Name 'mcp-stdio-show' -Tool 'memory_show' -Arguments @{ id = $mcpId } -DataDir $DirA
    Add-SweepRow 'mcp-stdio-show-hit' ($showTool.Run.Stdout -like "*$mcpMarker*") 'show contains marker'
    $updTool = Invoke-McpStdioTool -Name 'mcp-stdio-update' -Tool 'memory_update' -Arguments @{ id = $mcpId; title = 'mcp-sweep-marker-updated' } -DataDir $DirA
    Add-SweepRow 'mcp-stdio-update-envelope' ($null -ne $updTool.Envelope -and [string]$updTool.Envelope.command -eq 'update') ([string]$updTool.Envelope.command)
    $parentPrefix = ''
    if ($listTool.Envelope -and $listTool.Envelope.items) {
        $parentPrefix = [string](@($listTool.Envelope.items | Where-Object { [string]$_.name -ne $mcpId.Substring(0, [Math]::Min(8, $mcpId.Length)) } | Select-Object -First 1).name)
    }
    if (-not $parentPrefix) { $parentPrefix = $mcpId }
    $attachTool = Invoke-McpStdioTool -Name 'mcp-stdio-attach' -Tool 'memory_attach' -Arguments @{ id = $mcpId; parent = $parentPrefix } -DataDir $DirA
    $attachOk = $null -ne $attachTool.Envelope -and [string]$attachTool.Envelope.command -eq 'attach' -and [string]$attachTool.Envelope.status -ne 'fail'
    Add-SweepRow 'mcp-stdio-attach-envelope' $attachOk ([string]$attachTool.Envelope.status + ' parent=' + $parentPrefix)
    $histTool = Invoke-McpStdioTool -Name 'mcp-stdio-history' -Tool 'memory_history' -Arguments @{ id = $mcpId; limit = 10 } -DataDir $DirA
    Add-SweepRow 'mcp-stdio-history-envelope' ($null -ne $histTool.Envelope -and [string]$histTool.Envelope.command -eq 'history') ([string]$histTool.Envelope.command)
    $chainTool = Invoke-McpStdioTool -Name 'mcp-stdio-chain' -Tool 'memory_chain' -Arguments @{ id = $mcpId; depth = 2 } -DataDir $DirA
    Add-SweepRow 'mcp-stdio-chain-envelope' ($null -ne $chainTool.Envelope -and [string]$chainTool.Envelope.command -eq 'chain') ([string]$chainTool.Envelope.command)
    $markTool = Invoke-McpStdioTool -Name 'mcp-stdio-query-log-mark' -Tool 'memory_query_log_mark' -Arguments @{ ids = $mcpId; good = $true } -DataDir $DirA
    Add-SweepRow 'mcp-stdio-query-log-mark-ran' ($null -ne $markTool.Rpc) ([string]$markTool.Envelope.status)
    foreach ($deniedTool in @('memory_forget','memory_restore')) {
        $denied = Invoke-McpStdioTool -Name "mcp-stdio-default-$deniedTool" -Tool $deniedTool -Arguments @{ id = $mcpId } -DataDir $DirA
        Assert-Smoke "mcp-stdio-default-$deniedTool-denied" ($denied.Rpc.result.isError -eq $true -and $null -eq $denied.Envelope)
    }
    $defaultStdioRead = Invoke-McpStdioTool -Name 'mcp-stdio-default-denial-readback' -Tool 'memory_show' -Arguments @{ id = $mcpId } -DataDir $DirA
    Assert-Smoke 'mcp-stdio-default-denial-preserved-content' ([string]$defaultStdioRead.Envelope.details.entry.content -like "*$mcpMarker*")
    $forgetTool = Invoke-McpStdioTool -Name 'mcp-stdio-forget' -Tool 'memory_forget' -Arguments @{ id = $mcpId } -DataDir $DirA -AllowDestructiveMcpFixture
    Add-SweepRow 'mcp-stdio-forget-envelope' ($null -ne $forgetTool.Envelope -and [string]$forgetTool.Envelope.command -eq 'forget') ([string]$forgetTool.Envelope.command)
    $restoreTool = Invoke-McpStdioTool -Name 'mcp-stdio-restore' -Tool 'memory_restore' -Arguments @{ id = $mcpId } -DataDir $DirA -AllowDestructiveMcpFixture
    Add-SweepRow 'mcp-stdio-restore-envelope' ($null -ne $restoreTool.Envelope -and [string]$restoreTool.Envelope.command -eq 'restore') ([string]$restoreTool.Envelope.command)
} else {
    foreach ($n in @('mcp-stdio-show-hit','mcp-stdio-update-envelope','mcp-stdio-attach-envelope','mcp-stdio-history-envelope','mcp-stdio-chain-envelope','mcp-stdio-query-log-mark-ran','mcp-stdio-forget-envelope','mcp-stdio-restore-envelope')) {
        Add-SweepRow $n $false 'no remember id'
    }
}
$treeTool = Invoke-McpStdioTool -Name 'mcp-stdio-tree' -Tool 'memory_tree' -Arguments @{ outline = $true } -DataDir $DirA
Add-SweepRow 'mcp-stdio-tree-envelope' ($null -ne $treeTool.Envelope -and [string]$treeTool.Envelope.command -eq 'tree') ([string]$treeTool.Envelope.command)
$diaryTool = Invoke-McpStdioTool -Name 'mcp-stdio-diary' -Tool 'memory_diary' -Arguments @{ limit = 5 } -DataDir $DirA
Add-SweepRow 'mcp-stdio-diary-envelope' ($null -ne $diaryTool.Envelope -and [string]$diaryTool.Envelope.command -eq 'diary') ([string]$diaryTool.Envelope.command)
$badTool = Invoke-McpStdioTool -Name 'mcp-stdio-unknown-tool' -Tool 'memory_nope' -DataDir $DirA
$badIsError = $false
if ($badTool.Rpc -and $badTool.Rpc.result) { $badIsError = [bool]$badTool.Rpc.result.isError }
Add-SweepRow 'mcp-stdio-unknown-is-error' $badIsError 'unknown tool'
$missingContent = Invoke-McpStdioTool -Name 'mcp-stdio-remember-missing' -Tool 'memory_remember' -Arguments @{ title = 'x' } -DataDir $DirA
$missingIsError = $false
if ($missingContent.Rpc -and $missingContent.Rpc.result) { $missingIsError = [bool]$missingContent.Rpc.result.isError }
Add-SweepRow 'mcp-stdio-remember-missing-is-error' $missingIsError 'missing content'

if ($mcpUrl -and $mcpId) {
    $httpRemember = Invoke-McpHttpRpc 'mcp-http-recall' @{
        jsonrpc = '2.0'; id = 10; method = 'tools/call'
        params = @{ name = 'memory_recall'; arguments = @{ query = $mcpMarker; limit = 3 } }
    }
    $httpText = ''
    if ($httpRemember -and $httpRemember.result -and $httpRemember.result.content) {
        $httpText = [string]$httpRemember.result.content[0].text
    }
    Add-SweepRow 'mcp-http-recall-hit' ($httpText -like "*$mcpMarker*") 'http recall contains marker'
}

function Invoke-McpHttpTool([string]$Name, [string]$Tool, [hashtable]$Arguments, [string]$Command) {
    $rpc = Invoke-McpHttpRpc $Name @{ jsonrpc = '2.0'; id = $script:Step; method = 'tools/call'; params = @{ name = $Tool; arguments = $Arguments } }
    $envelope = Get-McpToolEnvelope $rpc
    Assert-Smoke "$Name-envelope" ($null -ne $envelope -and [string]$envelope.command -eq $Command -and [string]$envelope.status -in @('ok', 'warn'))
    return $envelope
}
$httpMarker = "mcp-http-business-$stamp"
$httpCreate = Invoke-McpHttpTool 'mcp-http-remember' 'memory_remember' @{ content = $httpMarker; title = "http-$stamp"; importance = 'important'; force = $true; parent = $id } 'remember'
$httpId = [string]$httpCreate.summary.id
Assert-Smoke 'mcp-http-remember-created-id' ([bool]$httpId)
foreach ($tool in @(
    @{ name = 'memory_status'; args = @{}; command = 'status' },
    @{ name = 'memory_taxonomy'; args = @{}; command = 'taxonomy' },
    @{ name = 'memory_list'; args = @{ limit = 100 }; command = 'list' },
    @{ name = 'memory_tree'; args = @{ from = $id; depth = 3 }; command = 'tree' },
    @{ name = 'memory_diary'; args = @{ limit = 100 }; command = 'diary' },
    @{ name = 'memory_history'; args = @{ id = $httpId; limit = 10 }; command = 'history' },
    @{ name = 'memory_chain'; args = @{ id = $httpId; depth = 2 }; command = 'chain' }
)) {
    Invoke-McpHttpTool "mcp-http-$($tool.name)" $tool.name $tool.args $tool.command | Out-Null
}
$httpShow = Invoke-McpHttpTool 'mcp-http-show' 'memory_show' @{ id = $httpId } 'show'
Assert-Smoke 'mcp-http-show-content' ([string]$httpShow.details.entry.content -eq $httpMarker)
Invoke-McpHttpTool 'mcp-http-update' 'memory_update' @{ id = $httpId; title = "http-updated-$stamp" } 'update' | Out-Null
$httpUpdated = Invoke-McpHttpTool 'mcp-http-update-readback' 'memory_show' @{ id = $httpId } 'show'
Assert-Smoke 'mcp-http-update-title-persisted' ([string]$httpUpdated.details.entry.title -eq "http-updated-$stamp")
Invoke-McpHttpTool 'mcp-http-attach' 'memory_attach' @{ id = $httpId; parent = $splitId } 'attach' | Out-Null
$httpParent = Invoke-McpHttpTool 'mcp-http-attach-readback' 'memory_show' @{ id = $httpId } 'show'
Assert-Smoke 'mcp-http-parent-persisted' ([string]$httpParent.details.entry.parent_id -eq $splitId)
$httpRecall = Invoke-McpHttpTool 'mcp-http-business-recall' 'memory_recall' @{ query = $httpMarker; limit = 5 } 'recall'
Assert-Smoke 'mcp-http-business-recall-hit' (($httpRecall | ConvertTo-Json -Depth 12) -like "*$httpMarker*")
$httpMark = Invoke-McpHttpTool 'mcp-http-query-log-mark' 'memory_query_log_mark' @{ ids = $httpId; good = $true } 'query-log'
Assert-Smoke 'mcp-http-query-log-marked' ([int]$httpMark.summary.marked -gt 0)
foreach ($deniedTool in @('memory_forget','memory_restore')) {
    $denied = Invoke-McpHttpRpc "mcp-http-default-$deniedTool" @{ jsonrpc='2.0'; id=$script:Step; method='tools/call'; params=@{ name=$deniedTool; arguments=@{id=$httpId} } }
    Assert-Smoke "mcp-http-default-$deniedTool-denied" ($denied.result.isError -eq $true -and $null -eq (Get-McpToolEnvelope $denied))
}
$defaultHttpRead = Invoke-McpHttpTool 'mcp-http-default-denial-readback' 'memory_show' @{ id=$httpId } 'show'
Assert-Smoke 'mcp-http-default-denial-preserved-content' ([string]$defaultHttpRead.details.entry.content -eq $httpMarker)
function Update-McpFixtureEndpoint {
    Get-OwnedRuntimeHealth $DirA | Out-Null
    $endpoint = Get-Content -LiteralPath (Join-Path $DirA 'runtime/endpoint.json') -Raw | ConvertFrom-Json
    $uri = [Uri][string]$endpoint.url
    if ($uri.Scheme -ne 'http' -or $uri.Host -notin @('127.0.0.1','localhost','::1')) { throw 'MCP fixture endpoint is not loopback.' }
    $script:origin = $uri.GetLeftPart([UriPartial]::Authority)
    $script:mcpUrl = "$script:origin/mcp"
    $script:mcpSse = "$script:origin/sse"
}
try {
    Invoke-Om -Name 'mcp-http-stop-before-explicit-policy' -ArgList @('--runtime-internal','--stop') -DataDir $DirA -Raw | Out-Null
    Invoke-Om -Name 'mcp-http-start-explicit-policy' -ArgList @('--json','status') -DataDir $DirA -AllowDestructiveMcpFixture | Out-Null
    Update-McpFixtureEndpoint
    $limited = Invoke-McpHttpRpc 'mcp-http-explicit-tools-list' @{jsonrpc='2.0';id=$script:Step;method='tools/list'}
    $limitedNames = @($limited.result.tools | ForEach-Object { [string]$_.name })
    $limitedWant = @('memory_show','memory_forget','memory_restore')
    Assert-Smoke 'mcp-http-explicit-policy-limited' ($limitedNames.Count -eq 3 -and @($limitedWant | Where-Object { $limitedNames -cnotcontains $_ }).Count -eq 0 -and @($limitedNames | Where-Object { $limitedWant -cnotcontains $_ }).Count -eq 0)
    Invoke-McpHttpTool 'mcp-http-forget' 'memory_forget' @{ id = $httpId } 'forget' | Out-Null
    Invoke-McpHttpTool 'mcp-http-restore' 'memory_restore' @{ id = $httpId } 'restore' | Out-Null
    $httpRestored = Invoke-McpHttpTool 'mcp-http-restore-readback' 'memory_show' @{ id = $httpId } 'show'
    Assert-Smoke 'mcp-http-restore-content-persisted' ([string]$httpRestored.details.entry.content -eq $httpMarker)
} finally {
    Invoke-Om -Name 'mcp-http-stop-explicit-policy' -ArgList @('--runtime-internal','--stop') -DataDir $DirA -Raw | Out-Null
    Invoke-Om -Name 'mcp-http-restart-default-policy' -ArgList @('--json','status') -DataDir $DirA | Out-Null
    Update-McpFixtureEndpoint
}
$restoredPolicy = Invoke-McpHttpRpc 'mcp-http-restored-default-list' @{jsonrpc='2.0';id=$script:Step;method='tools/list'}
$restoredNames = @($restoredPolicy.result.tools | ForEach-Object { [string]$_.name })
Assert-Smoke 'mcp-http-default-policy-restored' ($restoredNames.Count -eq 16 -and @($wantTools | Where-Object {$restoredNames -cnotcontains $_}).Count -eq 0 -and @($restoredNames | Where-Object {$wantTools -cnotcontains $_}).Count -eq 0)
$unknownHttp = Invoke-McpHttpRpc 'mcp-http-unknown-tool' @{ jsonrpc = '2.0'; id = 90; method = 'tools/call'; params = @{ name = 'memory_nope'; arguments = @{} } }
Assert-Smoke 'mcp-http-unknown-tool-is-error' ($unknownHttp.result.isError -eq $true)
Invoke-LocalSmoke -Name 'mcp-http-malformed-json' -Path '/mcp' -Payload '{' -RawBody -ExpectedStatus 400 | Out-Null
Invoke-LocalSmoke -Name 'mcp-http-foreign-origin' -Path '/mcp' -Payload @{} -Headers @{ Origin = 'https://example.invalid' } -ExpectedStatus 403 | Out-Null

# MCP writes schedule background indexing; report serialization must not act as its synchronization.
$m3InstallTimeout = Wait-OwnedModelIdle $DirA 1200
$m3Install = Invoke-Om -Name 'model-install-m3' -ArgList @('--json', 'model', 'install-m3') -DataDir $DirA -TimeoutSec $m3InstallTimeout
Assert-Smoke 'model-m3-installed-without-activation' ($m3Install.Ok -and $m3Install.Envelope.summary.activated -eq $false -and (Test-Path -LiteralPath (Join-Path $Root 'models/bge-m3/onnx/model_quantized.onnx')))
$m3Probe = Invoke-Om -Name 'model-probe-m3' -ArgList @('--json', 'model', 'probe', '--model', 'm3', '--text', $marker) -DataDir $DirA -TimeoutSec 600
Assert-Smoke 'model-m3-probe-success' ($m3Probe.Ok -and [string]$m3Probe.Envelope.status -eq 'ok')
$activateM3 = Invoke-Om -Name 'model-activate-m3' -ArgList @('--json', 'model', 'activate', 'm3') -DataDir $DirA -TimeoutSec 900
$m3Recall = Invoke-Om -Name 'model-m3-recall-existing-content' -ArgList @('--json', 'recall', $marker, '--limit', '5') -DataDir $DirA -TimeoutSec 300
$m3MarkerHits = @($m3Recall.Envelope.details | Where-Object { [string]$_.entry.content -like "*$marker*" })
Assert-Smoke 'model-m3-index-and-recall' ($activateM3.Ok -and [string]$activateM3.Envelope.summary.model -eq 'm3' -and [int]$activateM3.Envelope.summary.indexed -ge 0 -and $m3Recall.Ok -and [string]$m3Recall.Envelope.summary.embedding_model -eq 'm3' -and $m3MarkerHits.Count -gt 0)
$activateLegacy = Invoke-Om -Name 'model-activate-legacy-rejected' -ArgList @('--json', 'model', 'activate', 'legacy') -DataDir $DirA -ExpectExit @(2) -Raw
$m3AfterReject = Invoke-Om -Name 'model-m3-recall-after-retired-request' -ArgList @('--json', 'recall', $marker, '--limit', '5') -DataDir $DirA -TimeoutSec 300
$preservedContent = Invoke-Om -Name 'model-show-preserved-content' -ArgList @('--json', 'show', $httpId) -DataDir $DirA
Assert-Smoke 'model-retired-request-preserves-content' ($activateLegacy.Ok -and $m3AfterReject.Ok -and [string]$m3AfterReject.Envelope.summary.embedding_model -eq 'm3' -and $preservedContent.Ok -and [string]$preservedContent.Envelope.details.entry.content -ceq $httpMarker)
$revealed = Invoke-Om -Name 'secret-reveal-isolated' -ArgList @('--json', 'secret', '--reveal') -DataDir $DirA -Secret -AllowStatus @('warn')
Assert-Smoke 'secret-reveal-current-recovery-code' ($revealed.Ok -and [string]$revealed.Envelope.details.secret -eq $newSuper)
Invoke-Om -Name 'model-reset-cpu' -ArgList @('--json', 'model', 'reset-cpu') -DataDir $DirA -HostProfile | Out-Null
$resetEngine = Invoke-Om -Name 'model-reset-cpu-readback' -ArgList @('--json', 'model', 'engine') -DataDir $DirA
Assert-Smoke 'model-reset-cpu-engine' ($resetEngine.Ok -and [string]$resetEngine.Envelope.summary.engine -eq 'cpu')
Invoke-Om -Name 'runtime-stop' -ArgList @('--json', '--runtime-internal', '--stop') -DataDir $DirA -ExpectExit @(0, 2) -Raw | Out-Null
Stop-OurRuntimes
$uninstallRerank = Invoke-Om -Name 'model-uninstall-rerank-rejected' -ArgList @('--direct', '--json', 'model', 'uninstall-rerank') -DataDir $DirA -ExpectExit @(2) -Raw
Assert-Smoke 'model-rerank-command-rejected' ($uninstallRerank.Ok)
$uninstallBge = Invoke-Om -Name 'model-uninstall' -ArgList @('--direct', '--json', 'model', 'uninstall') -DataDir $DirA
Assert-Smoke 'model-bge-removed' ($uninstallBge.Ok -and $uninstallBge.Envelope.summary.removed -eq $true -and -not (Test-Path -LiteralPath (Join-Path $Root 'models/bge-m3')))
foreach ($required in $script:Catalog.required_assertions) {
    if (-not @($Rows | Where-Object { $_.Name -ceq $required -and $_.Ok -and $_.Status -ceq 'ok' }).Count) {
        Add-SweepRow "missing-required-$required" $false 'required positive assertion was not observed'
    }
}
Invoke-CloudCleanup
Write-SweepReport -Force

$failed = @($Rows | Where-Object { -not $_.Ok })
$passed = @($Rows | Where-Object { $_.Ok -and $_.Status -notin @('fail', 'skip') })
$lines = New-Object System.Collections.Generic.List[string]
$lines.Add("# CLI dev sweep $cliVersion") | Out-Null
