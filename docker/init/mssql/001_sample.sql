-- Sample schema used by db-mssql's integration tests. Run against the
-- `tableplus_rs_sample` database (created by docker/init/mssql/000_create_database.sql
-- which runs first) by the mssql-init sidecar container, since the
-- official SQL Server image has no init-script hook of its own.
USE tableplus_rs_sample;
GO

CREATE SCHEMA shop;
GO

CREATE TABLE shop.customers (
    id            INT IDENTITY(1,1) PRIMARY KEY,
    full_name     NVARCHAR(255) NOT NULL,
    email         NVARCHAR(255) NOT NULL UNIQUE,
    is_active     BIT NOT NULL DEFAULT 1,
    signed_up_at  DATETIME2 NOT NULL DEFAULT SYSUTCDATETIME()
);
GO

CREATE TABLE shop.orders (
    id            INT IDENTITY(1,1) PRIMARY KEY,
    customer_id   INT NOT NULL FOREIGN KEY REFERENCES shop.customers(id),
    total_cents   BIGINT NOT NULL,
    status        NVARCHAR(32) NOT NULL DEFAULT 'pending',
    placed_at     DATETIME2 NOT NULL DEFAULT SYSUTCDATETIME()
);
GO

CREATE INDEX idx_orders_customer_id ON shop.orders(customer_id);
GO

INSERT INTO shop.customers (full_name, email, is_active) VALUES
    ('Ada Lovelace', 'ada@example.com', 1),
    ('Grace Hopper', 'grace@example.com', 1),
    ('Alan Turing', 'alan@example.com', 0);
GO

INSERT INTO shop.orders (customer_id, total_cents, status) VALUES
    (1, 2599, 'shipped'),
    (1, 999, 'pending'),
    (2, 15000, 'shipped');
GO
