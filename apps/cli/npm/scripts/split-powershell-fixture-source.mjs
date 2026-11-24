import {readFileSync} from 'node:fs';

// Inspect the original PowerShell statements without running the fixture.
export function readSplitPowerShellFixture(url) {
  return readFileSync(url, 'utf8').replace(/^[ \t]*\. \(Join-Path \$PSScriptRoot '([\w-]+\.ps1)'\)[ \t]*\r?$/gm,
    (_, part) => readFileSync(new URL(part, url), 'utf8').trimEnd());
}
