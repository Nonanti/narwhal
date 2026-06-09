#!/usr/bin/env bash
# Build the isolated /tmp/narwhal-demo workspace used by the MCP
# demo recording. Creates a tiny SQLite e-commerce schema with
# customers, products, orders and order_items.
set -euo pipefail

ROOT="${NARWHAL_DEMO_ROOT:-/tmp/narwhal-demo}"
rm -rf "$ROOT"
mkdir -p "$ROOT/config/narwhal" "$ROOT/data" "$ROOT/cache"

DB="$ROOT/store.db"

sqlite3 "$DB" <<'EOF'
CREATE TABLE customers (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT UNIQUE NOT NULL,
    region TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE products (
    id INTEGER PRIMARY KEY,
    sku TEXT UNIQUE NOT NULL,
    name TEXT NOT NULL,
    price_cents INTEGER NOT NULL,
    stock INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers(id),
    total_cents INTEGER NOT NULL,
    status TEXT NOT NULL,
    placed_at TEXT NOT NULL
);
CREATE TABLE order_items (
    order_id INTEGER NOT NULL REFERENCES orders(id),
    product_id INTEGER NOT NULL REFERENCES products(id),
    qty INTEGER NOT NULL,
    PRIMARY KEY (order_id, product_id)
);

INSERT INTO customers (name, email, region) VALUES
  ('Ada Lovelace',   'ada@analytical.io',   'EU'),
  ('Grace Hopper',   'grace@cobol.dev',     'US'),
  ('Linus Torvalds', 'linus@kernel.org',    'EU'),
  ('Ken Thompson',   'ken@unix.bell',       'US'),
  ('Margaret Hamilton','margaret@apollo.gov','US'),
  ('Barbara Liskov', 'barbara@mit.edu',     'US'),
  ('Donald Knuth',   'don@stanford.edu',    'US'),
  ('Tim Berners-Lee','tim@w3.org',          'EU');

INSERT INTO products (sku, name, price_cents, stock) VALUES
  ('SKU-001','Mechanical Keyboard',  12900, 42),
  ('SKU-002','USB-C Hub',             3500, 120),
  ('SKU-003','27" 4K Monitor',       49900, 8),
  ('SKU-004','Noise-cancelling Headphones', 29900, 17),
  ('SKU-005','Standing Desk',        59900, 3);

INSERT INTO orders (customer_id, total_cents, status, placed_at) VALUES
  (1, 16400, 'paid',     '2026-06-01 09:14:22'),
  (2, 49900, 'paid',     '2026-06-02 11:02:10'),
  (3,  3500, 'shipped',  '2026-06-02 13:55:01'),
  (4, 29900, 'paid',     '2026-06-03 08:21:33'),
  (5, 12900, 'pending',  '2026-06-04 16:40:09'),
  (1, 59900, 'paid',     '2026-06-05 10:11:48'),
  (6,  6900, 'cancelled','2026-06-05 14:30:00'),
  (7, 79800, 'paid',     '2026-06-06 18:02:51'),
  (8, 29900, 'shipped',  '2026-06-07 09:05:14');

INSERT INTO order_items VALUES
  (1,1,1),(1,2,1),
  (2,3,1),
  (3,2,1),
  (4,4,1),
  (5,1,1),
  (6,5,1),
  (7,2,2),
  (8,1,1),(8,3,1),(8,2,1),
  (9,4,1);
EOF

cat > "$ROOT/config/narwhal/connections.toml" <<EOF
schema_version = 2

[[connection]]
id     = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
name   = "store"
driver = "sqlite"
[connection.params]
path = "$DB"
EOF

cat > "$ROOT/config/narwhal/config.toml" <<'EOF'
schema_version = 2
[editor]
mode = "vim"
EOF

echo "demo workspace ready at $ROOT"
echo "  db:          $DB"
echo "  config:      $ROOT/config/narwhal/"
echo "  customers:   $(sqlite3 "$DB" 'SELECT count(*) FROM customers')"
echo "  orders:      $(sqlite3 "$DB" 'SELECT count(*) FROM orders')"
