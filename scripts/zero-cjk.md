# Zero-CJK guard

`node scripts/check-guardrails.mjs` scans the checkout's Git-tracked files
without extension or i18n path exemptions. Valid UTF-8 text is always scanned,
including SVG, locale modules and vendored sources. Missing, unmerged, linked
or unknown binary files fail closed. The only binary classes currently needed
are typed PNG and WOFF2 assets in their actual Client2 asset directories and
two exact historical little-endian float fixtures. A text file renamed to
a binary extension is still scanned.

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

The inherited Client2 branding SVG comments were translated without editing
the rendering data. The existing injection disclosure arrows are represented
by ASCII escapes for the same two Unicode glyphs rather than adding a
production JavaScript allowance.

Run focused checker tests with `node --test scripts/check-zero-cjk.test.mjs`.
