-- Sample schema used by db-postgres's integration tests and for manually
-- exercising the app against a real server. Mirrors the shape of the
-- MySQL/MSSQL sample schemas so the three adapters can be tested the same
-- way.
CREATE SCHEMA IF NOT EXISTS shop;

CREATE TABLE shop.customers (
    id          SERIAL PRIMARY KEY,
    full_name   TEXT NOT NULL,
    email       TEXT UNIQUE NOT NULL,
    is_active   BOOLEAN NOT NULL DEFAULT TRUE,
    signed_up_at TIMESTAMP NOT NULL DEFAULT now()
);

CREATE TABLE shop.orders (
    id          SERIAL PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES shop.customers(id),
    total_cents BIGINT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',
    placed_at   TIMESTAMP NOT NULL DEFAULT now()
);

CREATE INDEX idx_orders_customer_id ON shop.orders(customer_id);

INSERT INTO shop.customers (full_name, email, is_active) VALUES
    ('Ada Lovelace', 'ada@example.com', TRUE),
    ('Grace Hopper', 'grace@example.com', TRUE),
    ('Alan Turing', 'alan@example.com', FALSE);

INSERT INTO shop.orders (customer_id, total_cents, status) VALUES
    (1, 2599, 'shipped'),
    (1, 999, 'pending'),
    (2, 15000, 'shipped');
