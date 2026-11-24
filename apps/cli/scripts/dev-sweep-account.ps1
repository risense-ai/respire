if (-not $BusinessOnly) {
$revisionAfterWrite = Invoke-Om -Name 'memory-revision-after-write-read' -ArgList @('--json', 'memory-revision') -DataDir $DirA
Assert-Smoke 'memory-revision-after-write' ($revisionBeforeWrite.Ok -and $revisionAfterWrite.Ok -and [string]$revisionBeforeWrite.Envelope.command -ceq 'memory-revision' -and [string]$revisionAfterWrite.Envelope.command -ceq 'memory-revision' -and [string]$revisionBeforeWrite.Envelope.summary.profile -ceq [string]$revisionAfterWrite.Envelope.summary.profile -and [string]$revisionBeforeWrite.Envelope.summary.revision -cne [string]$revisionAfterWrite.Envelope.summary.revision)
$short = $id.Substring(0, [Math]::Min(8, $id.Length))
Invoke-Om -Name 'remember-human' -ArgList @('remember', "human path $marker", '--title', "human-$stamp", '--force', '--importance', 'trivial') -DataDir $DirA -Raw -TimeoutSec 300 | Out-Null
$child = Invoke-Om -Name 'remember-parent' -ArgList @(
    '--json', 'remember', "child of $marker", '--title', "child-$stamp", '--parent', $short, '--force', '--importance', 'important', '--type', 'context'
) -DataDir $DirA -TimeoutSec 300
$childId = [string]$child.Envelope.summary.id
$disp = Invoke-Om -Name 'remember-disposable' -ArgList @(
    '--json', 'remember', "disposable $marker", '--title', "discard-$stamp", '--force', '--importance', 'important'
) -DataDir $DirA -TimeoutSec 300
$dispId = [string]$disp.Envelope.summary.id

$mergeA = Invoke-Om -Name 'merge-src-a' -ArgList @(
    '--json', 'remember', "merge source A $marker",
    '--title', "merge-a-$stamp", '--type', 'decision', '--importance', 'important', '--force', '--project', 'respire-cli'
) -DataDir $DirA -TimeoutSec 180
$mergeAId = [string]$mergeA.Envelope.summary.id
if (-not $mergeAId) { throw 'merge-src-a returned no id' }
$mergeShort = $mergeAId.Substring(0, [Math]::Min(8, $mergeAId.Length))
Invoke-Om -Name 'merge-src-b' -ArgList @(
    '--json', 'remember', "merge source B $marker",
    '--title', "merge-b-$stamp", '--type', 'decision', '--importance', 'important', '--force', '--project', 'respire-cli'
) -DataDir $DirA -TimeoutSec 180 | Out-Null
$merged = Invoke-Om -Name 'merge-ids-short-and-title' -ArgList @(
    '--json', 'remember', "merged $marker",
    '--title', "merge-result-$stamp", '--merge-ids', "$mergeShort,merge-b-$stamp",
    '--type', 'decision', '--importance', 'important', '--force', '--project', 'respire-cli'
) -DataDir $DirA -TimeoutSec 180
if (-not $merged.Ok) { throw 'merge-ids failed' }
if ([int]$merged.Envelope.summary.dropped -lt 2) {
    throw "merge-ids dropped=$($merged.Envelope.summary.dropped); the short id or taxonomy name was not removed"
}
$gone = Invoke-Om -Name 'merge-old-gone' -ArgList @('--json', 'show', $mergeShort) -DataDir $DirA -ExpectExit @(1, 2) -AllowStatus @('fail')
if ($gone.Exit -eq 0) { throw "the old short id $mergeShort still resolves after the merge, so the old entry is still there" }

# Full merge regressions: both input orders, a root and an external parent,
# surviving descendants, and rejection before deleting any source.
foreach ($rootCase in @($true, $false)) {
    foreach ($reverseCase in @($false, $true)) {
        $case = "merge-tree-$rootCase-$reverseCase"
        $expectedParent = if ($rootCase) { '' } else { $id }
        $argsA = @('--json', 'remember', "$case ancestor $marker", '--title', "$case-a-$stamp", '--importance', 'important', '--force')
        if ($expectedParent) { $argsA += @('--parent', $expectedParent) }
        $aResult = Invoke-Om -Name "$case-create-a" -ArgList $argsA -DataDir $DirA
        $aId = [string]$aResult.Envelope.summary.id
        $bResult = Invoke-Om -Name "$case-create-b" -ArgList @('--json', 'remember', "$case child $marker", '--title', "$case-b-$stamp", '--importance', 'important', '--parent', $aId) -DataDir $DirA
        $bId = [string]$bResult.Envelope.summary.id
        $leafResult = Invoke-Om -Name "$case-create-leaf" -ArgList @('--json', 'remember', "$case leaf $marker", '--title', "$case-leaf-$stamp", '--importance', 'important', '--parent', $bId) -DataDir $DirA
        $leafId = [string]$leafResult.Envelope.summary.id
        $mergeIds = if ($reverseCase) { "$bId,$aId" } else { "$aId,$bId" }
        $rejected = Invoke-Om -Name "$case-reject-deleted-parent" -ArgList @('--json', 'remember', "$case rejected", '--title', "$case-rejected", '--importance', 'important', '--merge-ids', $mergeIds, '--parent', $aId) -DataDir $DirA -ExpectExit @(1) -AllowStatus @('fail')
        if (-not $rejected.Ok) { throw "$case did not reject a merged parent" }
        Invoke-Om -Name "$case-source-survives" -ArgList @('--json', 'show', $aId) -DataDir $DirA | Out-Null
        $result = Invoke-Om -Name "$case-apply" -ArgList @('--json', 'remember', "$case combined $marker", '--title', "$case-result-$stamp", '--importance', 'important', '--merge-ids', $mergeIds) -DataDir $DirA
        if (-not $result.Ok -or [string]$result.Envelope.summary.parent -ne $expectedParent -or [int]$result.Envelope.summary.dropped -ne 2) { throw "$case wrong merge result" }
        $leafResult = Invoke-Om -Name "$case-leaf-parent" -ArgList @('--json', 'show', $leafId) -DataDir $DirA
        if ([string]$leafResult.Envelope.details.entry.parent_id -ne $expectedParent) { throw "$case lost descendant ancestry" }
        $treeAudit = Invoke-Om -Name "$case-audit" -ArgList @('--json', 'audit') -DataDir $DirA
        if ([int]$treeAudit.Envelope.summary.orphans -ne 0) { throw "$case left orphans" }
    }
}

$recA = Invoke-Om -Name 'recall' -ArgList @('--json', 'recall', $marker, '--limit', '3', '--project', 'respire-cli') -DataDir $DirA -TimeoutSec 180
if ($recA.Stdout -notlike "*$marker*") { throw "A recall did not return the marker $marker" }
Invoke-Om -Name 'recall-human' -ArgList @('recall', $marker, '--limit', '3') -DataDir $DirA -Raw -TimeoutSec 180 | Out-Null
Invoke-Om -Name 'recall-trace' -ArgList @('--json', 'recall', $marker, '--trace', '--limit', '3') -DataDir $DirA -TimeoutSec 180 | Out-Null
$filtered = Invoke-Om -Name 'recall-filtered-titles' -ArgList @('--json', 'recall', $marker, '--mode', 'fast', '--type', 'decision', '--project', 'respire-cli', '--titles', '--limit', '10') -DataDir $DirA
Assert-Smoke 'recall-filtered-marker-title' ($filtered.Ok -and $filtered.Stdout -like "*$title*")
Invoke-Om -Name 'list-since' -ArgList @('--json', 'list', '--since', '2000-01-01', '--limit', '100') -DataDir $DirA | Out-Null
Invoke-Om -Name 'list-since-resort' -ArgList @('--json', 'list', '--since-resort', '--limit', '100') -DataDir $DirA | Out-Null
$listSnapshot = Invoke-Om -Name 'list' -ArgList @('--json', 'list', '--limit', '10') -DataDir $DirA
Assert-Smoke 'list-profile-contract' ($listSnapshot.Ok -and [string]$listSnapshot.Envelope.summary.profile -ceq [string]$revisionInitial.Envelope.summary.profile)
Invoke-Om -Name 'list-human' -ArgList @('list', '--limit', '5') -DataDir $DirA -Raw | Out-Null
Invoke-Om -Name 'show' -ArgList @('--json', 'show', $short) -DataDir $DirA | Out-Null
Invoke-Om -Name 'show-human' -ArgList @('show', $short) -DataDir $DirA -Raw | Out-Null
Invoke-Om -Name 'chain' -ArgList @('--json', 'chain', $short, '--depth', '2') -DataDir $DirA | Out-Null
Invoke-Om -Name 'candidates' -ArgList @('--json', 'candidates', "dev sweep marker $marker") -DataDir $DirA -TimeoutSec 180 | Out-Null
Invoke-Om -Name 'update' -ArgList @('--json', 'update', $short, '--title', "sweep-updated-$stamp", '--content', "updated $marker") -DataDir $DirA -TimeoutSec 180 | Out-Null
Invoke-Om -Name 'retitle' -ArgList @('--json', 'retitle', $short, '--title', $title) -DataDir $DirA -TimeoutSec 180 | Out-Null
$retitleFile = Join-Path $Work 'retitle.json'
[System.IO.File]::WriteAllText($retitleFile, "[{`"id`":`"$id`",`"title`":`"$title`"}]", [Text.UTF8Encoding]::new($false))
Invoke-Om -Name 'retitle-many' -ArgList @('--json', 'retitle-many', $retitleFile) -DataDir $DirA -TimeoutSec 180 | Out-Null
$updated = Invoke-Om -Name 'update-readback' -ArgList @('--json', 'show', $id) -DataDir $DirA
Assert-Smoke 'update-title-content-persisted' ($updated.Ok -and [string]$updated.Envelope.details.entry.title -eq $title -and [string]$updated.Envelope.details.entry.content -eq "updated $marker")
if ($childId) {
    Invoke-Om -Name 'attach' -ArgList @('--json', 'attach', $childId, '--parent', $short) -DataDir $DirA | Out-Null
    Invoke-Om -Name 'demote' -ArgList @('--json', 'demote', $childId, '--parent', $short) -DataDir $DirA | Out-Null
    Invoke-Om -Name 'promote' -ArgList @('--json', 'promote', $childId) -DataDir $DirA | Out-Null
}
Invoke-Om -Name 'tree' -ArgList @('--json', 'tree', '--depth', '3') -DataDir $DirA | Out-Null
Invoke-Om -Name 'tree-outline' -ArgList @('--json', 'tree', '--outline') -DataDir $DirA | Out-Null
Invoke-Om -Name 'tree-from' -ArgList @('--json', 'tree', '--from', $short, '--depth', '2') -DataDir $DirA | Out-Null
Invoke-Om -Name 'tree-material' -ArgList @('--json', 'tree', '--material', $short) -DataDir $DirA | Out-Null
Invoke-Om -Name 'diary' -ArgList @('--json', 'diary', '--limit', '5') -DataDir $DirA | Out-Null
Invoke-Om -Name 'diary-today' -ArgList @('--json', 'diary', '--date', 'today') -DataDir $DirA | Out-Null
$diaryFiltered = Invoke-Om -Name 'diary-range-contains' -ArgList @('--json', 'diary', '--from', '2000-01-01', '--to', '2100-01-01', '--contains', $marker, '--limit', '100') -DataDir $DirA
Assert-Smoke 'diary-filter-contains-marker' ($diaryFiltered.Ok -and $diaryFiltered.Stdout -like "*$marker*")
Invoke-Om -Name 'history' -ArgList @('--json', 'history', $short, '--limit', '10') -DataDir $DirA | Out-Null
Invoke-Om -Name 'query-log' -ArgList @('--json', 'query-log', '--limit', '10') -DataDir $DirA | Out-Null
Invoke-Om -Name 'query-log-stats' -ArgList @('--json', 'query-log', '--stats') -DataDir $DirA | Out-Null
$markRecall = Invoke-Om -Name 'query-log-mark-candidates' -ArgList @('--json', 'recall', $marker, '--titles', '--limit', '3') -DataDir $DirA -TimeoutSec 180
$markCandidates = @($markRecall.Envelope.details)
if (-not $markRecall.Ok -or $markCandidates.Count -eq 0) { throw 'No fresh candidates for query-log prefix verification' }
$markId = [string]$markCandidates[0].id
if ($markId.Length -lt 8) { throw 'Recall did not return a usable candidate UUID' }
$marked = Invoke-Om -Name 'query-log-mark' -ArgList @('--json', 'query-log', 'mark', $markId.Substring(0, 8), '--good') -DataDir $DirA
if (-not $marked.Ok -or [int]$marked.Envelope.summary.marked -ne 1) { throw 'Fresh 8-character candidate prefix was not marked' }
Invoke-Om -Name 'audit' -ArgList @('--json', 'audit') -DataDir $DirA | Out-Null
Invoke-Om -Name 'resort-status' -ArgList @('--json', 'resort', '--status') -DataDir $DirA | Out-Null
Invoke-Om -Name 'resort-threshold' -ArgList @('--json', 'resort', '--threshold', '50') -DataDir $DirA | Out-Null
if ($childId) {
    $spec = (@{ ops = @(@{ id = $childId; parent = $id }) } | ConvertTo-Json -Compress)
    Invoke-Om -Name 'resort-dry' -ArgList @('--json', 'resort', '--spec', $spec) -DataDir $DirA -AllowStatus @('ok', 'pending', 'warn') -Note 'without --go it must be pending' | Out-Null
}
Invoke-Om -Name 'defrag' -ArgList @('--json', 'defrag', '--top', '5') -DataDir $DirA -TimeoutSec 180 | Out-Null
Invoke-Om -Name 'tree-cure' -ArgList @('--json', 'tree-cure', '--top', '5') -DataDir $DirA -TimeoutSec 180 | Out-Null
Invoke-Om -Name 'tree-float' -ArgList @('--json', 'tree-float') -DataDir $DirA | Out-Null
Invoke-Om -Name 'split-material' -ArgList @('--json', 'split', $short) -DataDir $DirA | Out-Null
$splitEntry = Invoke-Om -Name 'split-fixture-create' -ArgList @('--json', 'remember', "split fixture $stamp", '--title', "split-$stamp", '--parent', $id, '--importance', 'important', '--force') -DataDir $DirA
$splitId = [string]$splitEntry.Envelope.summary.id
Assert-Smoke 'split-fixture-id' ($splitEntry.Ok -and [bool]$splitId)
$splitSummary = "split outline $stamp"
$splitSpec = @{ summary = $splitSummary; items = @(
    @{ title = "split-a-$stamp"; content = "split child A $stamp"; kind = 'context'; tags = @('smoke') },
    @{ title = "split-b-$stamp"; content = "split child B $stamp"; kind = 'context'; tags = @('smoke') }
) } | ConvertTo-Json -Compress -Depth 8
$splitApply = Invoke-Om -Name 'split-go' -ArgList @('--json', 'split', $splitId, '--go', '--spec', $splitSpec) -DataDir $DirA
$splitRead = Invoke-Om -Name 'split-summary-readback' -ArgList @('--json', 'show', $splitId) -DataDir $DirA
$splitTree = Invoke-Om -Name 'split-children-readback' -ArgList @('--json', 'tree', '--from', $splitId, '--depth', '2') -DataDir $DirA
Assert-Smoke 'split-created-two-children' ($splitApply.Ok -and [int]$splitApply.Envelope.summary.created -eq 2 -and $splitRead.Ok -and [string]$splitRead.Envelope.details.entry.content -eq $splitSummary -and $splitTree.Stdout -like "*split-a-$stamp*" -and $splitTree.Stdout -like "*split-b-$stamp*")
if ($childId) {
    $applySpec = @{ ops = @(@{ id = $childId; parent = $splitId }) } | ConvertTo-Json -Compress -Depth 8
    $resortApply = Invoke-Om -Name 'resort-go' -ArgList @('--json', 'resort', '--go', '--spec', $applySpec) -DataDir $DirA
    $resortRead = Invoke-Om -Name 'resort-parent-readback' -ArgList @('--json', 'show', $childId) -DataDir $DirA
    Assert-Smoke 'resort-parent-persisted' ($resortApply.Ok -and $resortRead.Ok -and [string]$resortRead.Envelope.details.entry.parent_id -eq $splitId)
    $cureApply = Invoke-Om -Name 'tree-cure-attach' -ArgList @('--json', 'tree-cure', '--id', $childId, '--parent', $id) -DataDir $DirA
    $cureRead = Invoke-Om -Name 'tree-cure-parent-readback' -ArgList @('--json', 'show', $childId) -DataDir $DirA
    Assert-Smoke 'tree-cure-parent-persisted' ($cureApply.Ok -and $cureRead.Ok -and [string]$cureRead.Envelope.details.entry.parent_id -eq $id)
}
Invoke-Om -Name 'resort-reset' -ArgList @('--json', 'resort', '--reset') -DataDir $DirA | Out-Null
Invoke-Om -Name 'tree-float-go' -ArgList @('--json', 'tree-float', '--go') -DataDir $DirA | Out-Null
$structuralAudit = Invoke-Om -Name 'tree-mutations-audit' -ArgList @('--json', 'audit') -DataDir $DirA
Assert-Smoke 'tree-mutations-no-orphans' ($structuralAudit.Ok -and [int]$structuralAudit.Envelope.summary.orphans -eq 0)
Invoke-Om -Name 'tree-deepen' -ArgList @('--json', 'tree-deepen', '--root', $short) -DataDir $DirA -AllowStatus @('ok', 'fail') -Note 'a small tree must refuse deepening' | Out-Null
Invoke-Om -Name 'classify-plan' -ArgList @('--json', 'classify', '--plan', '--limit', '5') -DataDir $DirA | Out-Null
Invoke-Om -Name 'classify-dry-run' -ArgList @('--json', 'classify', '--dry-run', '--limit', '1') -DataDir $DirA | Out-Null
Invoke-Om -Name 'repack' -ArgList @('--json', 'repack') -DataDir $DirA -TimeoutSec 300 | Out-Null
$beforeReembed = Invoke-Om -Name 'reembed-before-readback' -ArgList @('--json', 'show', $id) -DataDir $DirA
$entryBeforeReembed = $beforeReembed.Envelope.details.entry | ConvertTo-Json -Depth 20 -Compress
$reembedTask = [Guid]::NewGuid().ToString('N')
$reembed = Invoke-Om -Name 'reembed' -ArgList @('--json', 'reembed') -DataDir $DirA -TimeoutSec 300 -ModelTaskId $reembedTask
$invalidProgress = @($reembed.Progress | Where-Object {
    [string]$_.id -cne $reembedTask -or $_.phase -notin @('load', 'index-load', 'index') -or $_.cancelled -ne $false `
        -or ($_.done -isnot [long] -and $_.done -isnot [int]) -or $_.done -lt 0 `
        -or ($null -ne $_.total -and (($_.total -isnot [long] -and $_.total -isnot [int]) -or $_.total -lt $_.done))
})
$script:ModelProgressEvidence.contract = @{
    reembed_ok = [bool]$reembed.Ok
    invalid_count = $invalidProgress.Count
    completed_inactive = [bool]$script:ModelProgressEvidence.completed_inactive
    return_object_count = @($reembed).Count
    returned_sample_count = @($reembed.Progress).Count
    invalid_id_count = @($reembed.Progress | Where-Object { [string]$_.id -cne $reembedTask }).Count
    invalid_phase_count = @($reembed.Progress | Where-Object { $_.phase -notin @('load', 'index-load', 'index') }).Count
    invalid_cancelled_count = @($reembed.Progress | Where-Object { $_.cancelled -ne $false }).Count
    invalid_done_count = @($reembed.Progress | Where-Object { ($_.done -isnot [long] -and $_.done -isnot [int]) -or $_.done -lt 0 }).Count
    invalid_total_count = @($reembed.Progress | Where-Object { $null -ne $_.total -and (($_.total -isnot [long] -and $_.total -isnot [int]) -or $_.total -lt $_.done) }).Count
}
Assert-Smoke 'reembed-progress-contract' (@($reembed).Count -eq 1 -and $reembed.Ok -and $invalidProgress.Count -eq 0 -and $script:ModelProgressEvidence.completed_inactive)
Assert-Smoke 'reembed-count-dimensions' (($reembed.Envelope.summary.reembedded -is [long] -or $reembed.Envelope.summary.reembedded -is [int]) -and [long]$reembed.Envelope.summary.reembedded -ge 0 -and [int]$reembed.Envelope.summary.dims -eq 1024)
$afterReembed = Invoke-Om -Name 'reembed-after-readback' -ArgList @('--json', 'show', $id) -DataDir $DirA
Assert-Smoke 'reembed-entry-preserved' ($beforeReembed.Ok -and $afterReembed.Ok -and [string]$afterReembed.Envelope.details.entry.id -ceq $id -and ($afterReembed.Envelope.details.entry | ConvertTo-Json -Depth 20 -Compress) -ceq $entryBeforeReembed)
$cachedReembed = Invoke-Om -Name 'reembed-cached' -ArgList @('--json', 'reembed') -DataDir $DirA -TimeoutSec 300
Assert-Smoke 'reembed-cached-zero' ($cachedReembed.Ok -and ($cachedReembed.Envelope.summary.reembedded -is [long] -or $cachedReembed.Envelope.summary.reembedded -is [int]) -and [long]$cachedReembed.Envelope.summary.reembedded -eq 0 -and [int]$cachedReembed.Envelope.summary.dims -eq 1024)
Invoke-Om -Name 'book-material' -ArgList @('--json', 'book-material', $short) -DataDir $DirA | Out-Null
Invoke-Om -Name 'portrait-material' -ArgList @('--json', 'portrait-material', '--limit', '10') -DataDir $DirA | Out-Null
Invoke-Om -Name 'passport' -ArgList @('--json', 'passport') -DataDir $DirA | Out-Null
$shareFile = Join-Path $Work 'share.txt'
Invoke-Om -Name 'share' -ArgList @('--json', 'share', '--root', $short, '--out', $shareFile) -DataDir $DirA | Out-Null
if (Test-Path $shareFile) {
    Invoke-Om -Name 'share-import-preview' -ArgList @('--json', 'share-import', $shareFile) -DataDir $DirA -AllowStatus @('ok', 'pending', 'warn') -Note 'a pending preview has status=pending and exit 2' | Out-Null
    Invoke-Om -Name 'share-import-go' -ArgList @('--json', 'share-import', $shareFile, '--go', '--force', '--title', "imported-$stamp") -DataDir $DirA | Out-Null
}
$exportFile = Join-Path $Work 'export.json'
$backupFile = Join-Path $Work 'backup.sqlite'
$missingOutput = Join-Path $Work 'missing-output/unused.json'
$modeConfigBefore = [IO.File]::ReadAllBytes((Join-Path $DirA 'client.json'))
foreach ($invalidOutput in @(
    @{ Name = 'export-missing-output'; Args = @('--json', 'export', $missingOutput) },
    @{ Name = 'backup-missing-output'; Args = @('--json', 'backup', $missingOutput) },
    @{ Name = 'agent-config-invalid-mode'; Args = @('--json', 'agent-config', '--set', 'workspace_mode=bogus') }
)) {
    $rejectedOutput = Invoke-Om -Name $invalidOutput.Name -ArgList $invalidOutput.Args -DataDir $DirA -AllowStatus @('fail')
    Assert-Smoke "$($invalidOutput.Name)-input-contract" ($rejectedOutput.Ok -and $rejectedOutput.Exit -eq 2 -and [string]$rejectedOutput.Envelope.summary.reason -eq 'invalid_input' -and [string]$rejectedOutput.Envelope.details.error_type -eq 'user')
}
Assert-Smoke 'invalid-output-does-not-create-directory' (-not (Test-Path -LiteralPath (Split-Path -Parent $missingOutput)))
Assert-Smoke 'invalid-mode-preserves-config' ([Convert]::ToBase64String([IO.File]::ReadAllBytes((Join-Path $DirA 'client.json'))) -ceq [Convert]::ToBase64String($modeConfigBefore))
Invoke-Om -Name 'export' -ArgList @('--json', 'export', $exportFile) -DataDir $DirA | Out-Null
if (-not (Test-Path $exportFile) -or (Get-Item $exportFile).Length -lt 10) { throw 'export wrote no non-empty file' }
Invoke-Om -Name 'backup' -ArgList @('--json', 'backup', $backupFile) -DataDir $DirA | Out-Null
Assert-Smoke 'backup-file-sqlite-header' ((Test-Path -LiteralPath $backupFile) -and (Get-Item -LiteralPath $backupFile).Length -gt 16 -and [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($backupFile)[0..15]) -eq "SQLite format 3`0")
$imported = Invoke-Om -Name 'import' -ArgList @('--json', 'import', $exportFile) -DataDir $DirA -TimeoutSec 300
if (-not $imported.Ok) { throw 'import failed' }
$evalFile = Join-Path $Work 'eval.jsonl'
[System.IO.File]::WriteAllText($evalFile, "{`"query`":`"$marker`",`"expect`":[`"$short`"]}`n", [Text.UTF8Encoding]::new($false))
$benchOut = Join-Path $Work 'bench.json'
Invoke-Om -Name 'bench-mine' -ArgList @('--json', 'bench', 'mine', '--out', (Join-Path $Work 'mined.jsonl'), '--limit', '5') -DataDir $DirA | Out-Null
Invoke-Om -Name 'bench-run' -ArgList @('--json', 'bench', 'run', $evalFile, '--save', $benchOut, '--verbose') -DataDir $DirA -TimeoutSec 180 | Out-Null
if (Test-Path $benchOut) {
    Invoke-Om -Name 'bench-baseline' -ArgList @('--json', 'bench', 'run', $evalFile, '--baseline', $benchOut) -DataDir $DirA -TimeoutSec 180 | Out-Null
}
Invoke-Om -Name 'keygen-refuse' -ArgList @('--json', 'keygen') -DataDir $DirA -AllowStatus @('fail') -Note 'a cloud key already exists, so overwriting must be refused' | Out-Null
Invoke-Om -Name 'secret-masked' -ArgList @('--json', 'secret') -DataDir $DirA -Secret | Out-Null
Invoke-Om -Name 'keys-export' -ArgList @('--json', 'keys-export', '--out', (Join-Path $Secrets 'keys.txt')) -DataDir $DirA -Secret | Out-Null
Invoke-Om -Name 'account-list' -ArgList @('--json', 'account', 'list') -DataDir $DirA | Out-Null
$otherUser = "oth$stamp"
$otherPass = New-Pass
$otherCloudAccount = Add-CloudRegistration $otherUser $otherPass
$otherReg = Invoke-Om -Name 'register-second-user' -ArgList @('--json', 'register', '--addr', $Server, '--user', $otherUser, '--pass', $otherPass) -DataDir $DirA -Secret -HostProfile
Confirm-CloudRegistration $otherCloudAccount $otherReg
if (-not $otherReg.Ok) { throw 'registering a second user failed while a main account exists; it must not report none in the local keyring.' }
if (-not $otherReg.Envelope.summary.super) { throw 'the second user register did not issue a new super' }
# Exercise host takeover while the actual fixture runtime is still serving.
Invoke-Om -Name 'account-live-initial-status' -ArgList @('--json', 'status') -DataDir $DirA | Out-Null
$otherHealth = Get-OwnedRuntimeHealth $DirA
$accountMain = Invoke-Om -Name 'account-back-after-second-register' -ArgList @('--json', 'account', 'use', 'main') -DataDir $DirA
$mainHealth = Get-OwnedRuntimeHealth $DirA
Assert-Smoke 'account-live-main-takeover' ($accountMain.Ok -and (Test-RuntimeChanged $otherHealth $mainHealth $DirA))
$mainEntry = Invoke-Om -Name 'account-live-main-readback' -ArgList @('--json', 'show', $id) -DataDir $DirA
Assert-Smoke 'account-live-main-library-preserved' ($mainEntry.Ok -and [string]$mainEntry.Envelope.details.entry.id -ceq $id -and [string]$mainEntry.Envelope.details.entry.content -ceq [string]$afterReembed.Envelope.details.entry.content)
$accountOther = Invoke-Om -Name 'account-live-shorthand' -ArgList @('--json', 'account', $otherUser) -DataDir $DirA
$switchedHealth = Get-OwnedRuntimeHealth $DirA
$otherStatus = Invoke-Om -Name 'account-live-other-status' -ArgList @('--json', 'status') -DataDir $DirA
Assert-Smoke 'account-live-shorthand-takeover' ($accountOther.Ok -and (Test-RuntimeChanged $mainHealth $switchedHealth (Join-Path $DirA "accounts/$otherUser")))
Assert-Smoke 'account-live-other-identity' ($otherStatus.Ok -and [string]$otherStatus.Envelope.summary.session.user -ceq $otherUser -and $otherStatus.Envelope.summary.unlocked -eq $true)
$profileConfig = [IO.File]::ReadAllBytes((Join-Path $DirA 'client.json'))
$rejectionSnapshot = Get-AccountRejectionSnapshot $DirA (Join-Path $DirA "accounts/$otherUser")
$invalidAccount = Invoke-Om -Name 'account-live-invalid-rejected' -ArgList @('--json', 'account', 'use', '../invalid') -DataDir $DirA -AllowStatus @('fail')
$rejectedStatus = Invoke-Om -Name 'account-live-rejected-status-readback' -ArgList @('--json', 'status') -DataDir $DirA
$rejectedHealth = Get-OwnedRuntimeHealth $DirA
# Keep the established coverage ID, but pre-validation must preserve, not replace, the service.
Assert-Smoke 'account-live-rejected-restores-service' ($invalidAccount.Ok -and $invalidAccount.Exit -eq 1 -and ($invalidAccount.Envelope.errors -join ' ') -match 'profile name allows' -and (Test-RuntimeUnchanged $switchedHealth $rejectedHealth (Join-Path $DirA "accounts/$otherUser")) -and $rejectedStatus.Ok -and [string]$rejectedStatus.Envelope.summary.session.user -ceq $otherUser -and $rejectedStatus.Envelope.summary.unlocked -eq $true -and (Get-AccountRejectionSnapshot $DirA (Join-Path $DirA "accounts/$otherUser")) -ceq $rejectionSnapshot -and [Convert]::ToBase64String([IO.File]::ReadAllBytes((Join-Path $DirA 'client.json'))) -ceq [Convert]::ToBase64String($profileConfig))
$clientSwitch = Invoke-Om -Name 'account-live-client-only-rejected' -ArgList @('--client-only', '--json', 'account', 'use', 'main') -DataDir $DirA -AllowStatus @('fail')
$clientHealth = Get-OwnedRuntimeHealth $DirA
Assert-Smoke 'account-live-client-only-preserves-host' ($clientSwitch.Ok -and $clientSwitch.Exit -eq 1 -and ($clientSwitch.Envelope.errors -join ' ') -match 'client.only' -and [int]$clientHealth.pid -eq [int]$rejectedHealth.pid -and [string]$clientHealth.data_dir -ceq [string]$rejectedHealth.data_dir -and [Convert]::ToBase64String([IO.File]::ReadAllBytes((Join-Path $DirA 'client.json'))) -ceq [Convert]::ToBase64String($profileConfig))
$accountRestore = Invoke-Om -Name 'account-live-restore-main' -ArgList @('--json', 'account', 'use', 'main') -DataDir $DirA
$restoredHealth = Get-OwnedRuntimeHealth $DirA
Assert-Smoke 'account-live-restore-main-takeover' ($accountRestore.Ok -and (Test-RuntimeChanged $clientHealth $restoredHealth $DirA))
$configOther = Invoke-Om -Name 'config-live-profile' -ArgList @('--json', 'config', '--data-dir', (Join-Path $DirA "accounts/$otherUser")) -DataDir $DirA
$configOtherHealth = Get-OwnedRuntimeHealth $DirA
Assert-Smoke 'config-live-profile-takeover' ($configOther.Ok -and (Test-RuntimeChanged $restoredHealth $configOtherHealth (Join-Path $DirA "accounts/$otherUser")))
$configMain = Invoke-Om -Name 'config-live-restore-main' -ArgList @('--json', 'config', '--data-dir', $DirA) -DataDir $DirA
$configMainHealth = Get-OwnedRuntimeHealth $DirA
Assert-Smoke 'config-live-restore-main-takeover' ($configMain.Ok -and (Test-RuntimeChanged $configOtherHealth $configMainHealth $DirA))
$accountRemove = Invoke-Om -Name 'account-remove-disposable' -ArgList @('--json', 'account', 'remove', $otherUser, '--yes') -DataDir $DirA -HostProfile
$accountAfterRemove = Invoke-Om -Name 'account-remove-list-readback' -ArgList @('--json', 'account', 'list') -DataDir $DirA
Assert-Smoke 'account-remove-profile-gone' ($accountRemove.Ok -and $accountAfterRemove.Ok -and -not (Test-Path -LiteralPath (Join-Path $DirA "accounts/$otherUser")) -and @($accountAfterRemove.Envelope.details.accounts | Where-Object { [string]$_.name -eq $otherUser }).Count -eq 0)
Invoke-Om -Name 'space-list' -ArgList @('--json', 'space', 'list') -DataDir $DirA | Out-Null
Invoke-Om -Name 'space-create' -ArgList @('--json', 'space', 'create', 'sweepspace') -DataDir $DirA -HostProfile | Out-Null
$spaceUser = "sps$stamp"
$spacePass = New-Pass
$spaceCloudAccount = Add-CloudRegistration $spaceUser $spacePass
$spaceReg = Invoke-Om -Name 'space-register' -ArgList @('--json', 'register', '--addr', $Server, '--user', $spaceUser, '--pass', $spacePass) -DataDir $DirA -Secret
Confirm-CloudRegistration $spaceCloudAccount $spaceReg
if (-not $spaceReg.Ok) { throw 'register failed on the space profile. An empty profile must issue a new super and must not report none in the local keyring.' }
if (-not $spaceReg.Envelope.summary.super) { throw 'space register did not issue a new super' }
$spaceMarker = "space-marker-$stamp"
$spaceWrite = Invoke-Om -Name 'space-owner-write' -ArgList @('--json', 'remember', $spaceMarker, '--title', $spaceMarker, '--importance', 'important', '--force') -DataDir $DirA
$spaceEntryId = [string]$spaceWrite.Envelope.summary.id
$spaceSync = Invoke-Om -Name 'space-owner-sync' -ArgList @('--json', 'sync') -DataDir $DirA
Assert-Smoke 'space-owner-entry-synced' ($spaceWrite.Ok -and [bool]$spaceEntryId -and $spaceSync.Ok -and [int]$spaceSync.Envelope.summary.pushed -gt 0)
$spaceHealth = Get-OwnedRuntimeHealth $DirA
$spaceMain = Invoke-Om -Name 'space-live-main' -ArgList @('--json', 'space', 'use', 'main') -DataDir $DirA
$spaceMainHealth = Get-OwnedRuntimeHealth $DirA
Assert-Smoke 'space-live-main-takeover' ($spaceMain.Ok -and (Test-RuntimeChanged $spaceHealth $spaceMainHealth $DirA))
$spaceReturn = Invoke-Om -Name 'space-live-return' -ArgList @('--json', 'space', 'use', 'sweepspace') -DataDir $DirA
$spaceReturnHealth = Get-OwnedRuntimeHealth $DirA
$spaceReadback = Invoke-Om -Name 'space-live-return-readback' -ArgList @('--json', 'show', $spaceEntryId) -DataDir $DirA
Assert-Smoke 'space-live-return-takeover' ($spaceReturn.Ok -and (Test-RuntimeChanged $spaceMainHealth $spaceReturnHealth (Join-Path $DirA 'accounts/sweepspace')))
Assert-Smoke 'space-live-library-preserved' ($spaceReadback.Ok -and [string]$spaceReadback.Envelope.details.entry.id -ceq $spaceEntryId -and [string]$spaceReadback.Envelope.details.entry.content -ceq $spaceMarker)
$spaceProfileConfig = [IO.File]::ReadAllBytes((Join-Path $DirA 'client.json'))
$spaceRejectionSnapshot = Get-AccountRejectionSnapshot $DirA (Join-Path $DirA 'accounts/sweepspace')
$missingSpace = "missing$stamp"
$spaceRejected = Invoke-Om -Name 'space-live-missing-rejected' -ArgList @('--json', 'space', 'use', $missingSpace) -DataDir $DirA -AllowStatus @('fail')
$spaceRejectedReadback = Invoke-Om -Name 'space-live-rejected-library-readback' -ArgList @('--json', 'show', $spaceEntryId) -DataDir $DirA
$spaceRejectedHealth = Get-OwnedRuntimeHealth $DirA
Assert-Smoke 'space-live-rejected-restores-service' ($spaceRejected.Ok -and $spaceRejected.Exit -eq 2 -and [string]$spaceRejected.Envelope.summary.reason -eq 'invalid_input' -and [string]$spaceRejected.Envelope.details.error_type -eq 'user' -and ($spaceRejected.Envelope.errors -join ' ') -match ('space "' + [regex]::Escape($missingSpace) + '" does not exist') -and -not (Test-Path -LiteralPath (Join-Path $DirA "accounts/$missingSpace")) -and (Test-RuntimeUnchanged $spaceReturnHealth $spaceRejectedHealth (Join-Path $DirA 'accounts/sweepspace')) -and $spaceRejectedReadback.Ok -and [string]$spaceRejectedReadback.Envelope.details.entry.id -ceq $spaceEntryId -and [string]$spaceRejectedReadback.Envelope.details.entry.content -ceq $spaceMarker -and (Get-AccountRejectionSnapshot $DirA (Join-Path $DirA 'accounts/sweepspace')) -ceq $spaceRejectionSnapshot -and [Convert]::ToBase64String([IO.File]::ReadAllBytes((Join-Path $DirA 'client.json'))) -ceq [Convert]::ToBase64String($spaceProfileConfig))
$inv = Invoke-Om -Name 'space-invite' -ArgList @('--json', 'space', 'invite', '--note', 'sweep') -DataDir $DirA -Secret
Invoke-Om -Name 'space-members' -ArgList @('--json', 'space', 'members') -DataDir $DirA | Out-Null
$inviteCode = ''
$memberSid = ''
if ($inv.Envelope.details.code) { $inviteCode = [string]$inv.Envelope.details.code }
if ($inv.Envelope.details.session_id) { $memberSid = [string]$inv.Envelope.details.session_id }
Assert-Smoke 'space-invite-code-session-present' ($inv.Ok -and [bool]$inviteCode -and [bool]$memberSid)
$DirC = Join-Path $Root 'c'
if ($inviteCode) {
    Invoke-Om -Name 'config-addr-c' -ArgList @('--json', 'config', '--addr', $Server) -DataDir $DirC | Out-Null
    Invoke-Om -Name 'space-join' -ArgList @('--json', 'space', 'join', $inviteCode) -DataDir $DirC -Secret -HostProfile | Out-Null
    Invoke-Om -Name 'space-use-c' -ArgList @('--json', 'space', 'use', 'sweepspace') -DataDir $DirC -HostProfile | Out-Null
    $stC = Invoke-Om -Name 'status-c' -ArgList @('--json', 'status') -DataDir $DirC
    $addrC = ''
    if ($stC.Envelope.summary.server_addr) { $addrC = [string]$stC.Envelope.summary.server_addr }
    if ($addrC -ne $Server) { throw "server_addr=$addrC after space join" }
    $spacePull = Invoke-Om -Name 'space-member-sync' -ArgList @('--json', 'sync') -DataDir $DirC
    $spaceRead = Invoke-Om -Name 'space-member-readback' -ArgList @('--json', 'show', $spaceEntryId) -DataDir $DirC
    Assert-Smoke 'space-member-can-read-owner-entry' ($spacePull.Ok -and $spaceRead.Ok -and [string]$spaceRead.Envelope.details.entry.content -eq $spaceMarker)
}
$readonlyTombstone = Invoke-Om -Name 'space-readonly-tombstone-create' -ArgList @('--json', 'remember', "readonly-tombstone-$stamp", '--title', 'readonly-tombstone', '--importance', 'important', '--force') -DataDir $DirA
$readonlyTombstoneId = [string]$readonlyTombstone.Envelope.summary.id
$readonlyDeleted = Invoke-Om -Name 'space-readonly-tombstone-delete' -ArgList @('--json', 'forget', $readonlyTombstoneId) -DataDir $DirA
$readonlySeedSync = Invoke-Om -Name 'space-readonly-tombstone-sync' -ArgList @('--json', 'sync') -DataDir $DirA
Assert-Smoke 'space-readonly-tombstone-ready' ($readonlyTombstone.Ok -and [bool]$readonlyTombstoneId -and $readonlyDeleted.Ok -and $readonlySeedSync.Ok)
$readonlyInvite = Invoke-Om -Name 'space-invite-readonly' -ArgList @('--json', 'space', 'invite', '--readonly', '--note', 'readonly-smoke') -DataDir $DirA -Secret -AllowStatus @('warn')
$readonlyCode = [string]$readonlyInvite.Envelope.details.code
Assert-Smoke 'space-readonly-invite-present' ($readonlyInvite.Ok -and $readonlyInvite.Envelope.details.readonly -eq $true -and [bool]$readonlyCode)
$readonlyDir = Join-Path $Root 'readonly-member'
Invoke-Om -Name 'space-readonly-config' -ArgList @('--json', 'config', '--addr', $Server, '--autosync', 'false') -DataDir $readonlyDir | Out-Null
$readonlyJoin = Invoke-Om -Name 'space-readonly-join' -ArgList @('--json', 'space', 'join', $readonlyCode) -DataDir $readonlyDir -Secret -HostProfile -AllowStatus @('warn')
Invoke-Om -Name 'space-readonly-use' -ArgList @('--json', 'space', 'use', 'sweepspace') -DataDir $readonlyDir -HostProfile | Out-Null
Invoke-Om -Name 'space-readonly-immediate-purge-config' -ArgList @('--json', 'agent-config', '--set', 'purge_days=0') -DataDir $readonlyDir | Out-Null
$readonlyPull = Invoke-Om -Name 'space-readonly-sync' -ArgList @('--json', 'sync') -DataDir $readonlyDir
Assert-Smoke 'space-readonly-no-auto-purge' ($readonlyPull.Ok -and [int]$readonlyPull.Envelope.summary.purged -eq 0 -and [int]$readonlyPull.Envelope.summary.pushed -eq 0 -and [int]$readonlyPull.Envelope.summary.pending -eq 0)
$readonlyRead = Invoke-Om -Name 'space-readonly-show' -ArgList @('--json', 'show', $spaceEntryId) -DataDir $readonlyDir
Assert-Smoke 'space-readonly-can-read' ($readonlyJoin.Ok -and $readonlyPull.Ok -and $readonlyRead.Ok -and [string]$readonlyRead.Envelope.details.entry.content -eq $spaceMarker)
$readonlyWrite = Invoke-Om -Name 'space-readonly-write-refused' -ArgList @('--json', 'remember', "forbidden-$stamp", '--title', 'must-not-write', '--force') -DataDir $readonlyDir -AllowStatus @('fail')
Assert-Smoke 'space-readonly-write-guard' ($readonlyWrite.Ok -and $readonlyWrite.Exit -eq 1 -and ($readonlyWrite.Envelope.errors -join ' ') -match 'read.only')
$readonlyUnlock = Invoke-Om -Name 'space-readonly-self-unlock-refused' -ArgList @('--json', 'agent-config', '--set', 'readonly=false') -DataDir $readonlyDir -AllowStatus @('fail')
Assert-Smoke 'space-readonly-self-unlock-guard' ($readonlyUnlock.Ok -and $readonlyUnlock.Exit -eq 1 -and ($readonlyUnlock.Envelope.errors -join ' ') -match 'read.only')
if ($memberSid) {
    $kick = Invoke-Om -Name 'space-kick' -ArgList @('--json', 'space', 'kick', '--session', $memberSid) -DataDir $DirA
    Assert-Smoke 'space-kick-real-member' ($kick.Ok -and [int]$kick.Envelope.summary.revoked -eq 1 -and [int]$kick.Envelope.summary.failed -eq 0)
    $kickedPull = Invoke-Om -Name 'space-kicked-member-cloud-denied' -ArgList @('--json', 'sync') -DataDir $DirC -AllowStatus @('fail')
    Assert-Smoke 'space-kicked-session-denied' ($kickedPull.Ok -and $kickedPull.Exit -eq 1 -and ($kickedPull.Envelope.errors -join ' ') -match 'unauthorized|token rejected|401')
}
$kickAll = Invoke-Om -Name 'space-kick-all' -ArgList @('--json', 'space', 'kick', '--all') -DataDir $DirA
Assert-Smoke 'space-kick-all-member-sessions' ($kickAll.Ok -and [int]$kickAll.Envelope.summary.revoked -ge 1 -and [int]$kickAll.Envelope.summary.failed -eq 0 -and [int]$kickAll.Envelope.summary.remaining -eq 0)
$readonlyKicked = Invoke-Om -Name 'space-kicked-readonly-cloud-denied' -ArgList @('--json', 'sync') -DataDir $readonlyDir -AllowStatus @('fail')
Assert-Smoke 'space-kicked-readonly-session-denied' ($readonlyKicked.Ok -and $readonlyKicked.Exit -eq 1 -and ($readonlyKicked.Envelope.errors -join ' ') -match 'unauthorized|token rejected|401')
Invoke-Om -Name 'runtime-stop-before-space-remove' -ArgList @('--runtime-internal', '--stop') -DataDir $DirA -Raw -ExpectExit @(0, 2) | Out-Null
Stop-OurRuntimes
Start-Sleep -Seconds 1
Invoke-Om -Name 'account-use-main' -ArgList @('--direct', '--json', 'account', 'use', 'main') -DataDir $DirA -Note 'web --stop may return while the process is still alive; end this runtime first and then use --direct' | Out-Null
Invoke-Om -Name 'space-remove' -ArgList @('--direct', '--json', 'space', 'remove', 'sweepspace', '--yes') -DataDir $DirA | Out-Null
Invoke-Om -Name 'root-create' -ArgList @('--json', 'root-create', "sweep-root-$stamp", '--content', 'disposable acceptance root', '--yes') -DataDir $DirA | Out-Null
$grant = Invoke-Om -Name 'grant-create' -ArgList @('--json', 'grant', 'create', '--root', $id, '--label', 'sweep') -DataDir $DirA -Secret
$grantList = Invoke-Om -Name 'grant-list' -ArgList @('--json', 'grant', 'list') -DataDir $DirA
$grantId = ''
if ($grant.Envelope.details.grant.id) { $grantId = [string]$grant.Envelope.details.grant.id }
elseif ($grant.Envelope.details.id) { $grantId = [string]$grant.Envelope.details.id }
if ($grantId) {
    Assert-Smoke 'grant-created-persisted' ($grant.Ok -and [string]$grant.Envelope.details.token -match '^[0-9a-f]{64}$' -and @($grantList.Envelope.details.grants | Where-Object { $_.id -eq $grantId -and $_.root_id -eq $id -and $_.revoked -eq $false }).Count -eq 1)
    $grantRevoke = Invoke-Om -Name 'grant-revoke' -ArgList @('--json', 'grant', 'revoke', $grantId.Substring(0, 8)) -DataDir $DirA
    $grantRevoked = Invoke-Om -Name 'grant-revoke-readback' -ArgList @('--json', 'grant', 'list') -DataDir $DirA
    Assert-Smoke 'grant-revocation-persisted' ($grantRevoke.Ok -and $grantRevoke.Envelope.details.revoked -eq $true -and @($grantRevoked.Envelope.details.grants | Where-Object { $_.id -eq $grantId -and $_.revoked -eq $true }).Count -eq 1)
} else {
    Assert-Smoke 'grant-created-persisted' $false
}
$grantRoot = Invoke-Om -Name 'grant-delete-root-fixture' -ArgList @('--json', 'remember', "grant-root-$stamp", '--title', "grant-root-$stamp", '--importance', 'important', '--force') -DataDir $DirA
$grantRootId = [string]$grantRoot.Envelope.summary.id
$deleteGrant = Invoke-Om -Name 'grant-delete-root-create' -ArgList @('--json', 'grant', 'create', '--root', $grantRootId, '--label', 'delete-root') -DataDir $DirA -Secret
$deleteGrantId = [string]$deleteGrant.Envelope.details.grant.id
Invoke-Om -Name 'grant-root-forget' -ArgList @('--json', 'forget', $grantRootId) -DataDir $DirA | Out-Null
$autoRevoked = Invoke-Om -Name 'grant-delete-root-readback' -ArgList @('--json', 'grant', 'list') -DataDir $DirA
Assert-Smoke 'grant-delete-root-auto-revoked' ($deleteGrant.Ok -and [bool]$deleteGrantId -and @($autoRevoked.Envelope.details.grants | Where-Object { $_.id -eq $deleteGrantId -and $_.revoked -eq $true }).Count -eq 1)
Invoke-Om -Name 'session-list' -ArgList @('--json', 'session', 'list') -DataDir $DirA -TimeoutSec 120 | Out-Null
Invoke-Om -Name 'session-revoke-bogus' -ArgList @('--json', 'session', 'revoke', '00000000-0000-0000-0000-000000000000') -DataDir $DirA -ExpectExit @(0, 1) -Note 'do not revoke the current session' | Out-Null
$retiredInstall = Invoke-Om -Name 'model-install-rerank' -ArgList @('--json', 'model', 'install-rerank') -DataDir $DirA -ExpectExit @(2) -Raw
}
