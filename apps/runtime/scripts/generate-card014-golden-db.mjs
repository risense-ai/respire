// Freeze a synthetic TEXT-ID 1.x schema and the existing historical ciphertext.
import { DatabaseSync } from 'node:sqlite';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, rmdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
const directory = new URL('../crates/storage/fixtures/', import.meta.url);
mkdirSync(directory, { recursive: true });
const scratch = mkdtempSync(new URL('golden-db-', directory));
const databasePath = join(scratch, 'legacy-1x.db');

try {
  const db = new DatabaseSync(databasePath);
  try {
    populate(db);
  } finally {
    db.close();
  }
  const lines = readFileSync(databasePath).toString('base64').match(/.{1,76}/g);
  writeFileSync(new URL('legacy-1x.db.b64', directory), `${lines.join('\n')}\n`, { flag: 'wx' });
} finally {
  rmSync(databasePath, { force: true });
  rmdirSync(scratch);
}

function populate(db) {
  db.exec(`
CREATE TABLE memories (
  id TEXT PRIMARY KEY, user TEXT NOT NULL, ciphertext TEXT NOT NULL,
  nonce TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  title TEXT NOT NULL, embedding_enc TEXT NOT NULL
);
CREATE TABLE memory_chunks(memory_id TEXT NOT NULL, seq INTEGER NOT NULL,
  embedding BLOB NOT NULL, PRIMARY KEY(memory_id,seq));
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE sync_remote_heads(id TEXT PRIMARY KEY, rev INTEGER NOT NULL);
CREATE TABLE sync_outbox (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, op_id TEXT NOT NULL UNIQUE, id TEXT NOT NULL,
  base_rev INTEGER, parent_op_id TEXT, user TEXT NOT NULL, ciphertext TEXT NOT NULL,
  nonce TEXT NOT NULL, embedding_enc TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL,
  deleted INTEGER NOT NULL, state TEXT NOT NULL DEFAULT 'pending'
);
CREATE TABLE query_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT, ts_unix INTEGER NOT NULL, ts TEXT NOT NULL,
  query TEXT NOT NULL, project TEXT NOT NULL DEFAULT '', scope TEXT NOT NULL DEFAULT '',
  hit_ids TEXT NOT NULL DEFAULT '[]', hit_scores TEXT NOT NULL DEFAULT '[]',
  adopted TEXT NOT NULL DEFAULT '[]'
);
INSERT INTO query_log(ts_unix,ts,query,hit_ids,hit_scores)
VALUES(1,'synthetic-time','golden-query','["legacy-memory-0001"]','[0.75]');
CREATE TRIGGER golden_legacy_sync AFTER INSERT ON memory_chunks
BEGIN INSERT OR IGNORE INTO meta VALUES('golden_trigger','retained'); END;
INSERT INTO meta VALUES('golden_meta','retained');
INSERT INTO memory_chunks VALUES('legacy-memory-0001',0,X'0000803f');
INSERT INTO sync_remote_heads VALUES('legacy-memory-0001',7);
`);
  const row = JSON.parse(readFileSync(new URL(
    '../crates/app/src/fixtures/legacy-memory-v1/legacy_stored_memory.json', import.meta.url)));
  db.prepare('INSERT INTO memories VALUES(?,?,?,?,?,?,?,?)').run(
    row.id, row.user, row.ciphertext, row.nonce, row.updated_at, row.updated_at,
    'Legacy fixture', row.embedding_enc);
  db.prepare(`INSERT INTO sync_outbox
    (seq,op_id,id,base_rev,user,ciphertext,nonce,embedding_enc,updated_at,deleted,state)
    VALUES(17,'golden-pending',?,7,?,?,?,?,?,0,'pending')`).run(
    row.id, row.user, row.ciphertext, row.nonce, row.embedding_enc, row.updated_at);
}
