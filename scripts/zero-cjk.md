# Zero-CJK guard

`node scripts/check-guardrails.mjs` scans the checkout's Git-tracked files
without extension or i18n path exemptions. Valid UTF-8 text is always scanned,
including SVG, locale modules and vendored sources. Missing, unmerged, linked
or unknown binary files fail closed. The only binary classes currently needed
are typed PNG and WOFF2 assets in their actual Client2 asset directories,
two exact historical little-endian float fixtures, and two exact SQLite
fixtures under `test-cases/fixtures/db/`: `comprehensive-synthetic.db` and
`respire-1x-synthetic.db`. Each database must match its reviewed path,
270336-byte length, 16-byte `SQLite format 3\0` header, and SHA-256 digest.
A text file renamed to a binary extension is still scanned.

The checker recognizes Han supplementary extensions, Hiragana, Katakana,
Hangul, Bopomofo and dedicated punctuation/compatibility blocks. It does not
mistake common English separators such as Latin middle dot for CJK text.
Its subject is raw source text; escape-sequence interpretation is not a
translation classifier.

The initial reviewed data roster was audited from monorepo
`0d37248329500f4daadf70812882707a4a221147`: compiled translations, localized
injection/prompt and installer data, compatibility lexicons/fixtures, and
already-imported vendor multilingual tests. Each entry in
`scripts/zero-cjk-reviewed-data.json` pins an exact repository path,
CJK-bearing line count, SHA-256 of the UTF-8 JSON array of those complete
lines in source order, and a reason. LF and CRLF are equivalent. English
edits on other lines do not invalidate a data line. A new/modified/repeated
CJK line cannot use an existing path exemption. Remove stale entries; only
update the manifest for independently reviewed language data as part of that
data change, never to hide untranslated implementation or comments.
The roster also pins the reviewed CJK lines in `compiled-quick-panel.json`,
`compiled-workspace.json`, `screens.json`, and `transport.json` under
`test-cases/fixtures/i18n/`.

The inherited Client2 branding SVG comments were translated without editing
the rendering data. The existing injection disclosure arrows are represented
by ASCII escapes for the same two Unicode glyphs rather than adding a
production JavaScript allowance.

Run focused checker tests with `node --test scripts/check-zero-cjk.test.mjs`.


### Required independent review for roster changes

The exact-line check does not infer which text is legitimate localization.
A PR that adds or changes a roster entry must stay unmerged until the
repository maintainer has evidence from a reviewer independent of the author.
The reviewer must inspect the exact candidate head, compare changed entries
against the target branch, and check the corresponding CJK lines in source.
Only genuine locale data or intentionally multilingual fixtures qualify;
implementation prose and comments do not. A new or relocated exception
needs the same check. Removing stale entries does not add an allowance.
Record the reviewed head and the paths in the PR's review evidence. If
the head changes, re-review the affected entries. A green CI result alone
is not independent approval; this repository procedure does not assume
CODEOWNERS or branch-rule settings which have not been verified.
