-- Sample schema used by db-mysql's integration tests. Mirrors the
-- Postgres/MSSQL sample schemas (minus a separate `shop` schema, since in
-- MySQL a schema *is* a database, and the container already creates one
-- for us via MYSQL_DATABASE).
CREATE TABLE customers (
    id            INT AUTO_INCREMENT PRIMARY KEY,
    full_name     VARCHAR(255) NOT NULL,
    email         VARCHAR(255) NOT NULL UNIQUE,
    is_active     BOOLEAN NOT NULL DEFAULT TRUE,
    signed_up_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE orders (
    id            INT AUTO_INCREMENT PRIMARY KEY,
    customer_id   INT NOT NULL,
    total_cents   BIGINT NOT NULL,
    status        VARCHAR(32) NOT NULL DEFAULT 'pending',
    placed_at     TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (customer_id) REFERENCES customers(id)
);

CREATE INDEX idx_orders_customer_id ON orders(customer_id);

INSERT INTO customers (full_name, email, is_active) VALUES
    ('Ada Lovelace', 'ada@example.com', TRUE),
    ('Grace Hopper', 'grace@example.com', TRUE),
    ('Alan Turing', 'alan@example.com', FALSE);

INSERT INTO orders (customer_id, total_cents, status) VALUES
    (1, 2599, 'shipped'),
    (1, 999, 'pending'),
    (2, 15000, 'shipped');
