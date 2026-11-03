// Reproduce the production rolling-update order using the store's actual SQL.
// argv[2] is an isolated installation of @electric-sql/pglite (Postgres in WASM).
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { PGlite } = require(process.argv[2]);

const source = fs.readFileSync('crates/store/src/inbox.rs', 'utf8');
const implementation = source.slice(source.indexOf('impl InboxStore for PgStore'));
function method(name) {
  const start = implementation.indexOf(`async fn ${name}(`);
  assert(start >= 0, `missing method ${name}`);
  const next = implementation.indexOf('\n    async fn ', start + 1);
  return implementation.slice(start, next < 0 ? undefined : next);
}
function sql(text) {
  return [...text.matchAll(/"((?:[^"\\]|\\.)*)"/g)]
    .map(m => m[1]).filter(s => /^(SELECT|INSERT|UPDATE|DELETE)\b/.test(s));
}

(async () => {
  const db = new PGlite();
  try {
    await db.exec(fs.readFileSync('crates/store/migrations/0011_inbox.sql', 'utf8'));
    const saveIndex = sql(method('save_inbox_index'));
    const saveWrites = sql(source.slice(source.indexOf('async fn save_writes(')));
    const upsertNode = saveWrites.find(s => s.startsWith('INSERT INTO inbox_pulls'));
    const reads = sql(method('inbox_open'));
    const due = sql(method('inbox_repos_due'))[0];
    const node = {
      number: 1, baseRefName: 'main', updatedAt: '2026-11-03T00:00:00Z',
      // The old binary's INBOX_FIELDS does not ask for defaultBranchRef.
      repository: { nameWithOwner: 'o/r' },
    };
    async function oldIndex() {
      await db.exec('BEGIN');
      await db.query(saveIndex[0], [1, 'o/r']);
      await db.query(upsertNode, [1, 'o/r', 1, 0, 'open', 'ada', [], [], [], null, node, true]);
      await db.query(saveIndex[1], [1, 'o/r', [1]]);
      await db.exec('COMMIT');
    }
    await oldIndex();
    // New pod connects/migrates before taking the queue lock from the old pod.
    await db.exec(fs.readFileSync('crates/store/migrations/0021_inbox_default_branch.sql', 'utf8'));
    assert.equal(Object.values((await db.query(reads[0], [1, 'o/r'])).rows[0])[0], false);
    // While the new pod becomes ready, the old leader finishes another full index.
    await oldIndex();
    const ready = Object.values((await db.query(reads[0], [1, 'o/r'])).rows[0])[0];
    const open = (await db.query(reads[1], [1, 'o/r'])).rows;
    assert.equal((await db.query(due, [86400])).rows.length, 0,
      'the old index postpones the next full index for a day');
    assert.ok(!ready || open.length === 1,
      `rolling deploy must retain the live fallback or return PR #1; got ready=${ready}, open=${JSON.stringify(open)}`);
  } finally {
    await db.close();
  }
})().catch(e => { console.error(e); process.exitCode = 1; });
