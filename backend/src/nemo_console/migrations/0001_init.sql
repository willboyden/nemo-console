CREATE TABLE keys (
  id TEXT PRIMARY KEY,
  role TEXT NOT NULL CHECK (role IN ('admin','viewer')),
  sha256 TEXT NOT NULL UNIQUE,
  created REAL NOT NULL,
  note TEXT NOT NULL DEFAULT ''
);
CREATE TABLE audit (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts REAL NOT NULL,
  actor TEXT NOT NULL,
  role TEXT NOT NULL,
  method TEXT NOT NULL,
  path TEXT NOT NULL,
  system TEXT,
  status INTEGER NOT NULL,
  params TEXT NOT NULL
);
CREATE INDEX audit_system ON audit(system, id);
CREATE TRIGGER audit_no_update BEFORE UPDATE ON audit BEGIN SELECT RAISE(ABORT, 'audit is append-only'); END;
CREATE TRIGGER audit_no_delete BEFORE DELETE ON audit BEGIN SELECT RAISE(ABORT, 'audit is append-only'); END;
CREATE TABLE revisions (
  id TEXT PRIMARY KEY,
  system TEXT NOT NULL,
  file_id TEXT NOT NULL,
  ts REAL NOT NULL,
  actor TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  note TEXT NOT NULL DEFAULT '',
  content BLOB NOT NULL
);
CREATE INDEX revisions_file ON revisions(system, file_id, ts);
CREATE TABLE rollup (
  system TEXT NOT NULL,
  series TEXT NOT NULL,
  bucket INTEGER NOT NULL,
  avg REAL NOT NULL,
  max REAL NOT NULL,
  n INTEGER NOT NULL,
  PRIMARY KEY (system, series, bucket)
);
CREATE TABLE settings (k TEXT PRIMARY KEY, v TEXT NOT NULL);
