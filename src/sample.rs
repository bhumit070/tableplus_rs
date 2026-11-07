//! The sample database (design.md 24.3, "Try the sample database"): a small
//! SQLite shop — customers, products, orders, about 2,000 rows — written to
//! the config folder on first use. No password, no network, so the grid,
//! staged edits and the query editor can be tried in the first minute.

use std::fs::{create_dir_all, remove_file, write};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::db::{Cancel, Connection, ConnectionConfig, Driver, EnvTag};
use crate::store::config_dir;

/// Where the sample database lives.
pub fn path() -> PathBuf {
    config_dir().join("sample-shop.sqlite")
}

/// A saved connection for it (not yet stored).
pub fn connection() -> ConnectionConfig {
    let mut cfg = ConnectionConfig::new(Driver::Sqlite);
    cfg.name = "Sample shop".into();
    cfg.tag = EnvTag::Local;
    cfg.file = path().to_string_lossy().into();
    cfg
}

/// The tables and rows, generated in SQL so nothing binary is bundled.
const SCRIPT: &str = "
create table customers (
  id integer primary key,
  name text not null,
  email text not null unique,
  country char(2) not null,
  vip boolean not null default false,
  joined_at text not null
);
create table products (
  id integer primary key,
  name text not null,
  category text not null check (category in ('books','games','garden','kitchen','music')),
  price numeric(10,2) not null,
  in_stock integer not null default 0
);
create table orders (
  id integer primary key,
  customer_id integer not null references customers(id),
  product_id integer not null references products(id),
  quantity integer not null,
  status text not null check (status in ('pending','paid','shipped','refunded')),
  total numeric(12,2) not null,
  created_at text not null
);
create index orders_customer on orders(customer_id);
create view big_orders as select * from orders where total > 500;

with recursive n(i) as (select 0 union all select i + 1 from n where i < 199),
  first(names) as (select json_array('Priya','Marcus','Ana','Kenji','Amara','Lucas','Mei','Omar',
    'Sofia','Arjun','Chloe','Diego','Fatima','Hiro','Ines','Jonas','Leila','Mateo','Nadia','Tariq')),
  last(names) as (select json_array('Raman','Lee','Souza','Watanabe','Okafor','Martin','Chen',
    'Haddad','Rossi','Mehta'))
insert into customers (id, name, email, country, vip, joined_at)
select i + 1,
  json_extract(first.names, '$[' || (i % 20) || ']') || ' ' ||
    json_extract(last.names, '$[' || ((i / 20) % 10) || ']'),
  lower(json_extract(first.names, '$[' || (i % 20) || ']')) || '.' ||
    lower(json_extract(last.names, '$[' || ((i / 20) % 10) || ']')) || (i / 200 + i) || '@example.com',
  json_extract('[\"IN\",\"US\",\"BR\",\"JP\",\"NG\",\"DE\",\"FR\",\"GB\"]', '$[' || (i % 8) || ']'),
  (i % 7 = 0),
  date('2024-01-01', '+' || ((i * 37) % 700) || ' days')
from n, first, last;

with recursive n(i) as (select 0 union all select i + 1 from n where i < 49),
  adj(words) as (select json_array('Classic','Compact','Deluxe','Everyday','Handmade','Modern',
    'Pocket','Vintage','Wireless','Wooden')),
  noun(words) as (select json_array('Atlas','Board Game','Cookbook','Headphones','Kettle')),
  cat(words) as (select json_array('books','games','music','kitchen','garden'))
insert into products (id, name, category, price, in_stock)
select i + 1,
  json_extract(adj.words, '$[' || (i % 10) || ']') || ' ' ||
    json_extract(noun.words, '$[' || ((i / 10) % 5) || ']'),
  json_extract(cat.words, '$[' || ((i / 10) % 5) || ']'),
  round(4.99 + ((i * 7919) % 9500) / 100.0, 2),
  (i * 13) % 120
from n, adj, noun, cat;

with recursive n(i) as (select 0 union all select i + 1 from n where i < 1749)
insert into orders (id, customer_id, product_id, quantity, status, total, created_at)
select 10001 + i,
  (i * 37) % 200 + 1,
  (i * 13) % 50 + 1,
  i % 4 + 1,
  json_extract('[\"paid\",\"shipped\",\"pending\",\"paid\",\"shipped\",\"refunded\",\"paid\"]',
    '$[' || (i % 7) || ']'),
  round((i % 4 + 1) * (select price from products where id = (i * 13) % 50 + 1), 2),
  datetime('2026-01-01', '+' || ((i * 5) % 310) || ' days', '+' || ((i * 17) % 1440) || ' minutes')
from n;
";

/// Writes the sample database at `file` unless it's already there.
pub async fn ensure(file: &Path) -> Result<()> {
    if file.exists() {
        return Ok(());
    }
    if let Some(dir) = file.parent() {
        create_dir_all(dir).context("creating the config folder")?;
    }
    write(file, b"").context("creating the sample database file")?;
    let mut cfg = ConnectionConfig::new(Driver::Sqlite);
    cfg.file = file.to_string_lossy().into();
    let result = async {
        let conn = Connection::connect(&cfg).await?;
        conn.execute_cancellable(SCRIPT, &Cancel::default()).await?;
        conn.close().await;
        Ok(())
    }
    .await;
    // A half-written sample would be reused next time: start over instead.
    if result.is_err() {
        let _ = remove_file(file);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime;
    use futures::executor::block_on;
    use std::env::temp_dir;
    use uuid::Uuid;

    #[test]
    fn sample_has_rows_in_every_table() {
        let file = temp_dir().join(format!("tabula-sample-{}.sqlite", Uuid::new_v4()));
        let check = file.clone();
        let counts = block_on(runtime::spawn(async move {
            ensure(&check).await?;
            let mut cfg = ConnectionConfig::new(Driver::Sqlite);
            cfg.file = check.to_string_lossy().into();
            let conn = Connection::connect(&cfg).await?;
            let result = conn
                .execute(
                    "select (select count(*) from customers), (select count(*) from products), \
                     (select count(*) from orders), \
                     (select count(*) from orders o join products p on p.id = o.product_id)",
                )
                .await?;
            conn.close().await;
            Ok(result[0].rows[0].clone())
        }))
        .unwrap();
        let counts: Vec<String> = counts.into_iter().map(|c| c.unwrap_or_default()).collect();
        assert_eq!(counts, ["200", "50", "1750", "1750"]);
        // A second call keeps the existing file.
        let again = file.clone();
        block_on(runtime::spawn(async move { ensure(&again).await })).unwrap();
        let _ = remove_file(file);
    }
}
