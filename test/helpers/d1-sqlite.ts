// Minimal D1Database shim over node:sqlite so the D1 repository SQL is exercised
// against real SQLite in unit tests (wrangler/miniflare is used for the e2e smoke run).
import { readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';

type Value = string | number | null | bigint | Uint8Array;

class Stmt {
  params: Value[] = [];
  constructor(
    private readonly db: DatabaseSync,
    readonly sql: string,
  ) {}
  bind(...args: unknown[]): Stmt {
    const s = new Stmt(this.db, this.sql);
    s.params = args.map((a) => (a === undefined ? null : typeof a === 'boolean' ? Number(a) : (a as Value)));
    return s;
  }
  async first<T>(col?: string): Promise<T | null> {
    const row = this.db.prepare(this.sql).get(...this.params) as Record<string, unknown> | undefined;
    if (!row) return null;
    return (col ? row[col] : { ...row }) as T;
  }
  async all<T>(): Promise<{ results: T[]; success: true; meta: object }> {
    const rows = this.db.prepare(this.sql).all(...this.params) as T[];
    return { results: rows.map((r) => ({ ...r })), success: true, meta: {} };
  }
  async run(): Promise<{ success: true; meta: object }> {
    this.db.prepare(this.sql).run(...this.params);
    return { success: true, meta: {} };
  }
  runSync(): void {
    this.db.prepare(this.sql).run(...this.params);
  }
}

export function createSqliteD1(migrationPath = 'migrations/0001_init.sql'): D1Database {
  const db = new DatabaseSync(':memory:');
  db.exec('PRAGMA foreign_keys = ON;');
  db.exec(readFileSync(migrationPath, 'utf8'));
  const d1 = {
    prepare: (sql: string) => new Stmt(db, sql),
    async batch(stmts: Stmt[]) {
      db.exec('BEGIN');
      try {
        for (const s of stmts) s.runSync();
        db.exec('COMMIT');
      } catch (e) {
        db.exec('ROLLBACK');
        throw e;
      }
      return stmts.map(() => ({ success: true, results: [], meta: {} }));
    },
    async exec(sql: string) {
      db.exec(sql);
      return { count: 0, duration: 0 };
    },
  };
  return d1 as unknown as D1Database;
}
