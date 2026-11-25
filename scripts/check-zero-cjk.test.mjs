import assert from "node:assert/strict";
import { test } from "node:test";
import { cjkProjection, scanCjk } from "./check-zero-cjk.mjs";

const bytes = text => Buffer.from(text);
const payload = String.fromCodePoint(0x20000);
const name = "apps/example/src/i18n/approved-data.js";
const original = "const label = '" + payload + "';";
const pin = new Map([[name, {lines:1, sha256:cjkProjection(original).digest}]]);

test("recognizes common and supplementary Han, Kana, Hangul, Bopomofo and CJK punctuation", () => {
  for (const cp of [0x4e2d, 0x20000, 0x2ebf0, 0x30000, 0x3347f, 0x3042, 0x30a2,
    0x30fc, 0x1b000, 0xff76, 0xac00, 0x1100, 0x3105, 0x31a0, 0x3001, 0xfe3f, 0xff1a]) {
    const hit = cjkProjection("x" + String.fromCodePoint(cp)).hits[0];
    assert.equal(hit?.point, cp, "missing U+" + cp.toString(16));
  }
});

test("ordinary typography and Latin punctuation are not CJK", () => {
  const plain = "Caf\u00e9 \u00b7 \u00b1 \u2014 \u2019 \u2026 \u2192";
  assert.equal(cjkProjection(plain).hits.length, 0);
});

test("allows only exact reviewed content at the exact path, not generic i18n or locale names", () => {
  assert.deepEqual(scanCjk([[name, bytes("// English edit\r\n" + original + "\r\n")]], pin).reports, []);
  for (const path of ["src/fake/i18n/payload.js", "docs/locales/text.md", "tools/config_zh.json", "vendor/new.rs"]) {
    assert.match(scanCjk([[path, bytes(original)]], pin).reports[0], /unreviewed CJK text/);
  }
  for (const changed of [original + "\n// " + payload, original + "\n" + original,
    original.replace("label", "tool"), ""]) {
    assert.ok(scanCjk([[name, bytes(changed)]], pin).reports.length > 0);
  }
});

test("unknown binary and text disguised as asset still fail", () => {
  const png = "apps/client2/internal/desktop/frontend/icons/utf8.png";
  assert.match(scanCjk([[png, bytes(original)]], new Map()).reports[0], /unreviewed CJK text/);
  assert.match(scanCjk([["src/code.rs", Buffer.from([255, 0, 1])]], new Map()).reports[0], /malformed UTF-8/);
  assert.match(scanCjk([[png, Buffer.from([255, 0, 1])]], new Map()).reports[0], /unclassified binary/);
});

test("publisher path does not exempt text in SVG badge comments", () => {
  assert.match(scanCjk([["apps/client2/internal/desktop/frontend/brand/icon.svg",
    bytes("<svg>\n<!-- " + payload + " -->\n</svg>")]], new Map()).reports[0], /:2: unreviewed CJK/);
});

test("tracked filenames use the same language policy even when text is clean", () => {
  assert.match(scanCjk([["docs/" + payload + ".md", bytes("English")]], new Map()).reports[0],
    /CJK in tracked filename/);
});
