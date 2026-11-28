#!/usr/bin/env node
// Idempotent initializer and status synchronizer for the risense-ai organization GitHub Project v2 board.
// Usage:
//   node process/scripts/sync-project-board.mjs         # Dry-run & token scope check
//   node process/scripts/sync-project-board.mjs --apply # Create/reuse Project & sync monorepo cards

import { execFileSync } from "node:child_process";
import { ACTIVE_CARD_REPO, fetchAllCards } from "./lib/cards.mjs";

const OWNER = "risense-ai";
const PROJECT_TITLE = "Respire Transformation (001-006)";
const apply = process.argv.includes("--apply");

function runGh(args, allowFail = false) {
  try {
    const out = execFileSync("gh", args, { encoding: "utf8", stdio: ["pipe", "pipe", "pipe"] });
    return { ok: true, out: out.trim(), err: "" };
  } catch (e) {
    if (allowFail) {
      return {
        ok: false,
        out: String(e.stdout || "").trim(),
        err: String(e.stderr || e.message || "").trim(),
      };
    }
    throw e;
  }
}

const listRes = runGh(["project", "list", "--owner", OWNER, "--format", "json"], true);
if (!listRes.ok) {
  if (/read:project|missing required scopes/i.test(listRes.err)) {
    console.error("Current gh token lacks GitHub Project scopes (read:project / project).");
    console.error("Ask the human Maintainer to run these two commands in a terminal:");
    console.error("  1. gh auth refresh -s project,read:project");
    console.error("  2. node process/scripts/sync-project-board.mjs --apply");
    process.exit(2);
  }
  console.error("Failed to list organization projects:", listRes.err);
  process.exit(1);
}

const projects = JSON.parse(listRes.out).projects || [];
let project = projects.find((p) => p.title === PROJECT_TITLE);

if (!project) {
  if (!apply) {
    console.log(`[dry-run] Would create org Project "${PROJECT_TITLE}" under ${OWNER}`);
    process.exit(0);
  }
  const created = runGh([
    "project",
    "create",
    "--owner",
    OWNER,
    "--title",
    PROJECT_TITLE,
    "--format",
    "json",
  ]);
  project = JSON.parse(created.out);
  console.log(`Created Project #${project.number}: ${project.url}`);
} else {
  console.log(`Using existing Project #${project.number}: ${project.url}`);
}

const itemsRes = runGh([
  "project",
  "item-list",
  String(project.number),
  "--owner",
  OWNER,
  "--limit",
  "200",
  "--format",
  "json",
]);
const existingUrls = new Set(
  (JSON.parse(itemsRes.out).items || []).map((it) => it.content?.url).filter(Boolean)
);

const fieldsRes = runGh([
  "project",
  "field-list",
  String(project.number),
  "--owner",
  OWNER,
  "--format",
  "json",
]);
const statusField = (JSON.parse(fieldsRes.out).fields || []).find((f) => f.name === "Status");
const statusOptByName = new Map(
  (statusField?.options || []).map((o) => [o.name.toLowerCase(), o.id])
);

function targetStatusName(card) {
  if (card.state === "closed") return "Done";
  if (card.labels.has("agent:in-progress") || card.labels.has("agent:verify")) return "In Progress";
  return "Todo";
}

let added = 0;
let updated = 0;

const { cards } = fetchAllCards((args) => runGh(args).out, [ACTIVE_CARD_REPO]);
for (const card of cards) {
  if (existingUrls.has(card.url)) continue;
  if (apply) {
    runGh(["project", "item-add", String(project.number), "--owner", OWNER, "--url", card.url]);
    added += 1;
  } else {
    console.log(`[dry-run] Would add ${card.ref} (${card.title})`);
  }
}

if (apply && statusField && project.id) {
  const refreshed =
    JSON.parse(
      runGh([
        "project",
        "item-list",
        String(project.number),
        "--owner",
        OWNER,
        "--limit",
        "200",
        "--format",
        "json",
      ]).out
    ).items || [];

  const cardsByUrl = new Map(cards.map((card) => [card.url, card]));

  for (const item of refreshed) {
    const card = cardsByUrl.get(item.content?.url);
    if (!card) continue;
    const wantName = targetStatusName(card);
    const wantOptId = statusOptByName.get(wantName.toLowerCase());
    if (!wantOptId || item.status === wantName) continue;
    runGh([
      "project",
      "item-edit",
      "--id",
      item.id,
      "--project-id",
      project.id,
      "--field-id",
      statusField.id,
      "--single-select-option-id",
      wantOptId,
    ]);
    updated += 1;
  }
}

console.log(
  apply
    ? `Project #${project.number} sync complete: added ${added}, updated status ${updated}.`
    : "Dry-run check complete. Pass --apply to execute sync."
);
