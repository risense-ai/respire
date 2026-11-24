# Expand the scripts under test as source only. Never run their live fixture steps.
function Get-SplitFixtureSource([string]$Path) {
    $source = [IO.File]::ReadAllText($Path)
    return [regex]::Replace($source, '(?m)^[ \t]*\. \(Join-Path \$PSScriptRoot ''([\w-]+\.ps1)''\)[ \t]*\r?$', {
        param($match)
        $part = Join-Path (Split-Path -Parent $Path) $match.Groups[1].Value
        [IO.File]::ReadAllText($part).TrimEnd([char]13, [char]10)
    })
}
