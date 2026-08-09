# Reviewing a Postgres schema change

Work through these in order. Stop at the first one that fails and report it —
a migration that takes a long lock is worse than a migration that is late.

## 1. Lock risk

Identify the lock each statement takes and how long it holds it.

- `ALTER TABLE ... ADD COLUMN` with a non-volatile default is metadata-only on
  PG 11+; with a volatile default it rewrites the table.
- `CREATE INDEX` takes a `SHARE` lock and blocks writes. Use
  `CREATE INDEX CONCURRENTLY`, and note that it cannot run inside a
  transaction — so it needs its own migration.
- `ALTER TABLE ... SET NOT NULL` scans the whole table. Add a `CHECK (col IS
  NOT NULL) NOT VALID` constraint, `VALIDATE` it, then set `NOT NULL`.
- Anything taking `ACCESS EXCLUSIVE` on a hot table needs a stated plan for
  what happens if it blocks behind a long-running query.

## 2. Reversibility

Every migration needs a down path, or an explicit note saying why it has none.
Dropping a column is not reversible: prefer a two-step deploy that stops
writing to it first.

## 3. Indexes

- Does an existing index already cover this query as a prefix?
- Is the new index actually selective, or is it a full scan with extra steps?
- Composite index column order follows the query's equality-then-range shape.

## 4. Query plans

Ask for `EXPLAIN (ANALYZE, BUFFERS)` on the query the change is meant to help,
before and after. A plan that improves on an empty table proves nothing —
check it against production-shaped row counts.

## 5. Data migration

Backfills run in batches with a bound on rows per statement, not as one
`UPDATE`. Say how long the whole backfill takes and whether it is resumable.
