#!/usr/bin/env bash
# Seed the SQLite demo database used by docs/img/demo.tape.
# Idempotent: drops and recreates /tmp/narwhal-demo/blog.db.

set -euo pipefail

DEMO_DIR=/tmp/narwhal-demo
DB="$DEMO_DIR/blog.db"

mkdir -p "$DEMO_DIR"
rm -f "$DB"

sqlite3 "$DB" <<'SQL'
CREATE TABLE authors (
  id     INTEGER PRIMARY KEY,
  name   TEXT NOT NULL,
  email  TEXT NOT NULL UNIQUE,
  joined DATE NOT NULL DEFAULT CURRENT_DATE
);

CREATE TABLE posts (
  id         INTEGER PRIMARY KEY,
  author_id  INTEGER NOT NULL REFERENCES authors(id),
  title      TEXT NOT NULL,
  body       TEXT NOT NULL,
  tags       TEXT NOT NULL DEFAULT '[]',
  views      INTEGER NOT NULL DEFAULT 0,
  created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE comments (
  id        INTEGER PRIMARY KEY,
  post_id   INTEGER NOT NULL REFERENCES posts(id),
  author    TEXT NOT NULL,
  body      TEXT NOT NULL,
  posted_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_posts_author  ON posts(author_id);
CREATE INDEX idx_comments_post ON comments(post_id);

INSERT INTO authors (name, email) VALUES
  ('Alice Kim',  'alice@example.com'),
  ('Berkant',    'berkant@example.com'),
  ('Chen Wei',   'chen@example.com'),
  ('Diego Ruiz', 'diego@example.com'),
  ('Elif Demir', 'elif@example.com');

INSERT INTO posts (author_id, title, body, tags, views) VALUES
  (1, 'Why Rust',         'Memory safety without GC',          '["rust","systems"]',      1240),
  (2, 'Building narwhal', 'A TUI DB client journey',           '["rust","tui","db"]',     3580),
  (2, 'MCP for ops',      'AI agents over real schemas',       '["ai","mcp","ops"]',      2110),
  (3, 'ClickHouse 101',   'OLAP basics',                       '["olap","clickhouse"]',    890),
  (4, 'pg_stat hacks',    'Tuning Postgres at scale',          '["postgres","perf"]',     4220),
  (5, 'SQLite in 2026',   'Still the right default',           '["sqlite"]',              1670);

INSERT INTO comments (post_id, author, body) VALUES
  (2, 'reader1', 'When does it go on crates.io?'),
  (2, 'reader2', 'Vim mode just works.'),
  (3, 'devops',  'MCP changed how I script DB ops.'),
  (5, 'dba',     'Still the king of embedded.');
SQL

mkdir -p "$DEMO_DIR/config/narwhal/plugins"

cat > "$DEMO_DIR/config/narwhal/connections.toml" <<TOML
[[connection]]
id = "11111111-1111-1111-1111-111111111111"
name = "blog"
driver = "sqlite"

[connection.params]
path = "$DB"
TOML

cat > "$DEMO_DIR/config/narwhal/config.toml" <<'TOML'
[theme]
preset = "default"
TOML

echo "seeded $DB"
echo "config at $DEMO_DIR/config/narwhal/"
