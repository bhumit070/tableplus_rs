//! End-to-end checks against real servers. SQLite always runs; the others
//! run when TABULA_IT=1 and expect the throwaway containers from
//! compose.yaml (`docker compose up -d --wait`) on these ports:
//!   pg 55432 (postgres/test), mysql 53306 (root/test, db app),
//!   redis 56379, mongo 57017.

use super::*;
use std::sync::Arc;
use tokio::{join, spawn, time::sleep, time::timeout};
use uuid::Uuid;

fn it_enabled() -> bool {
    std::env::var("TABULA_IT").is_ok()
}

fn cfg(driver: Driver) -> ConnectionConfig {
    let mut c = ConnectionConfig::new(driver);
    c.host = "127.0.0.1".into();
    match driver {
        Driver::Postgres => {
            c.port = 55432;
            c.user = "postgres".into();
            c.password = "test".into();
            c.database = "postgres".into();
        }
        Driver::MySql => {
            c.port = 53306;
            c.user = "root".into();
            c.password = "test".into();
            c.database = "app".into();
        }
        Driver::Redis => c.port = 56379,
        Driver::Mongo => {
            c.port = 57017;
            c.database = "tabula_it".into();
        }
        Driver::Sqlite => {
            let p = std::env::temp_dir().join(format!("tabula-it-{}.db", c.id));
            // An empty file is a valid new SQLite database.
            std::fs::write(&p, b"").unwrap();
            c.file = p.to_string_lossy().into();
        }
    }
    c
}

fn obj(conn_objects: &[DbObject], name: &str) -> DbObject {
    conn_objects
        .iter()
        .find(|o| o.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("{name} not listed"))
}

fn kv(pairs: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.map(String::from)))
        .collect()
}

async fn row_count(c: &Connection, obj: &DbObject) -> Option<u64> {
    let q = TableQuery {
        find: String::new(),
        object: obj.clone(),
        filters: vec![],
        order: None,
        limit: 1,
        offset: 0,
        match_any: false,
    };
    c.fetch_table(&q).await.unwrap().1
}

/// Shared SQL scenario: DDL, listing, structure, staged commit, filters, sort.
async fn sql_scenario(driver: Driver) {
    let c = Connection::connect(&cfg(driver)).await.expect("connect");
    let ddl = match driver {
        Driver::Postgres => {
            "drop table if exists it_people; create table it_people (id serial primary key, name text not null, age int, active boolean default true, meta jsonb)"
        }
        Driver::MySql => {
            "drop table if exists it_people; create table it_people (id int auto_increment primary key, name varchar(100) not null, age int, active boolean default true, meta json)"
        }
        _ => {
            "drop table if exists it_people; create table it_people (id integer primary key autoincrement, name text not null, age int, active boolean default 1, meta text)"
        }
    };
    let r = c.execute(ddl).await.expect("ddl");
    assert_eq!(r.len(), 2);

    let objects = c.list_objects().await.unwrap();
    let t = obj(&objects, "it_people");
    let cols = c.all_columns().await.unwrap();
    assert!(
        cols.iter().any(|(_, t, c)| t == "it_people" && c == "name"),
        "{cols:?}"
    );
    let st = c.structure(&t).await.unwrap();
    let names: Vec<_> = st.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "name", "age", "active", "meta"]);
    assert!(
        st.columns[0].is_pk && !st.columns[1].is_pk,
        "{:?}",
        st.columns
    );

    // Insert three rows (one with a quote and a NULL), via staged changes.
    let ins = |n: &str, a: Option<&str>| Change::Insert {
        values: kv(&[("id", None), ("name", Some(n)), ("age", a)]),
    };
    let changes = vec![
        ins("Ada", Some("36")),
        ins("O'Brien", None),
        ins("Linus", Some("54")),
    ];
    let preview = c.preview(&t, &st.columns, &changes);
    assert!(preview.contains("'O''Brien'"), "{preview}");
    c.commit(&t, &st.columns, &changes).await.expect("insert");

    let q = |filters: Vec<Filter>, order: Option<(String, bool)>| TableQuery {
        find: String::new(),
        object: t.clone(),
        filters,
        order,
        limit: 100,
        offset: 0,
        match_any: false,
    };
    let (res, total) = c
        .fetch_table(&q(vec![], Some(("age".into(), false))))
        .await
        .unwrap();
    assert_eq!(total, Some(3));
    assert_eq!(res.columns[1], "name");
    let ages: Vec<_> = res.rows.iter().map(|r| r[2].clone()).collect();
    // NULL sorts first on desc in pg, last in mysql/sqlite; just check order of non-null.
    let non_null: Vec<_> = ages.iter().flatten().cloned().collect();
    assert_eq!(non_null, ["54", "36"]);
    let obrien = res
        .rows
        .iter()
        .find(|r| r[1].as_deref() == Some("O'Brien"))
        .unwrap();
    assert_eq!(obrien[2], None);
    let ada_id = res
        .rows
        .iter()
        .find(|r| r[1].as_deref() == Some("Ada"))
        .unwrap()[0]
        .clone();

    // Update by PK, then delete by PK, in one transaction.
    let linus_id = res
        .rows
        .iter()
        .find(|r| r[1].as_deref() == Some("Linus"))
        .unwrap()[0]
        .clone();
    let changes = vec![
        Change::Update {
            key: vec![("id".into(), ada_id.clone())],
            set: kv(&[("age", Some("37")), ("meta", Some(r#"{"lang":"en"}"#))]),
        },
        Change::Delete {
            key: vec![("id".into(), linus_id)],
        },
    ];
    c.commit(&t, &st.columns, &changes)
        .await
        .expect("update+delete");

    // Filters: contains (case-insensitive) and is null.
    let (res, total) = c
        .fetch_table(&q(
            vec![Filter {
                column: "name".into(),
                op: FilterOp::Contains,
                value: "ada".into(),
                value2: String::new(),
            }],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(total, Some(1));
    assert_eq!(res.rows[0][2].as_deref(), Some("37"));
    assert!(res.rows[0][4].as_deref().unwrap().contains("lang"));
    if driver == Driver::Postgres {
        assert_eq!(res.rows[0][3].as_deref(), Some("true"));
    }
    let (_, total) = c
        .fetch_table(&q(
            vec![Filter {
                column: "age".into(),
                op: FilterOp::IsNull,
                value: String::new(),
                value2: String::new(),
            }],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(total, Some(1));

    // The newer operators (design.md answer #18), and "any".
    let only = |column: &str, op: FilterOp, value: &str, value2: &str| Filter {
        column: column.into(),
        op,
        value: value.into(),
        value2: value2.into(),
    };
    for (filters, any, expected) in [
        (
            vec![only("name", FilterOp::In, "Ada, O'Brien", "")],
            false,
            2,
        ),
        (vec![only("age", FilterOp::Between, "30", "40")], false, 1),
        (
            vec![only("name", FilterOp::StartsWith, "o'b", "")],
            false,
            1,
        ),
        (vec![only("name", FilterOp::EndsWith, "DA", "")], false, 1),
        (
            vec![
                only("name", FilterOp::Eq, "Ada", ""),
                only("age", FilterOp::IsNull, "", ""),
            ],
            true,
            2,
        ),
    ] {
        let mut query = q(filters, None);
        query.match_any = any;
        let (_, total) = c.fetch_table(&query).await.unwrap();
        assert_eq!(
            total,
            Some(expected),
            "{driver:?} {:?}",
            query.filters[0].op
        );
    }
    if driver != Driver::Sqlite {
        let (_, total) = c
            .fetch_table(&q(
                vec![only("name", FilterOp::Regex, "^[A-Z]da$", "")],
                None,
            ))
            .await
            .unwrap();
        assert_eq!(total, Some(1), "{driver:?} regex");
    }

    // An update that matches nothing must fail and roll back the whole batch.
    let bad = vec![
        Change::Update {
            key: vec![("id".into(), ada_id.clone())],
            set: kv(&[("age", Some("99"))]),
        },
        Change::Update {
            key: vec![("id".into(), Some("424242".into()))],
            set: kv(&[("age", Some("1"))]),
        },
    ];
    assert!(c.commit(&t, &st.columns, &bad).await.is_err());
    let (res, _) = c
        .fetch_table(&q(
            vec![Filter {
                column: "id".into(),
                op: FilterOp::Eq,
                value: ada_id.clone().unwrap(),
                value2: String::new(),
            }],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.rows[0][2].as_deref(), Some("37"), "rolled back");

    // Editor: multi-statement, empty result keeps headers, affected count.
    let r = c
        .execute("select name from it_people where 1 = 0; update it_people set age = age + 1 where age is not null")
        .await
        .unwrap();
    assert_eq!(r[0].columns, ["name"]);
    assert!(r[0].rows.is_empty());
    assert_eq!(r[1].affected, Some(1));

    // Editor transactions run on one connection (GitHub review #1):
    // ROLLBACK really undoes the DELETE, on every backend.
    c.execute("BEGIN; DELETE FROM it_people; ROLLBACK")
        .await
        .unwrap();
    assert_eq!(row_count(&c, &t).await, Some(2), "rolled back");
    // A batch that leaves a transaction open is rolled back and reported.
    let err = c.execute("BEGIN; DELETE FROM it_people").await.unwrap_err();
    assert!(
        format!("{err:#}").contains("BEGIN without COMMIT"),
        "{err:#}"
    );
    assert_eq!(
        row_count(&c, &t).await,
        Some(2),
        "open transaction not applied"
    );
    // COMMIT keeps changes made inside the transaction.
    c.execute("BEGIN; UPDATE it_people SET age = 40 WHERE age IS NULL; COMMIT")
        .await
        .unwrap();
    // A failing statement mid-transaction rolls the batch back.
    let err = c
        .execute("BEGIN; DELETE FROM it_people; SELECT * FROM no_such_table")
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("statement 3 of 3"), "{err:#}");
    assert_eq!(row_count(&c, &t).await, Some(2));

    // Database list and truncate / drop.
    let dbs = c.databases().await.unwrap();
    assert!(!dbs.is_empty());
    c.truncate(&t).await.unwrap();
    assert_eq!(c.fetch_table(&q(vec![], None)).await.unwrap().1, Some(0));
    c.drop_object(&t).await.unwrap();
    assert!(
        !c.list_objects()
            .await
            .unwrap()
            .iter()
            .any(|o| o.name == "it_people")
    );
    c.close().await;
}

/// Review #13: a missing file is reported, not silently created.
#[tokio::test]
async fn sqlite_missing_file_is_an_error() {
    let mut c = ConnectionConfig::new(Driver::Sqlite);
    let p = std::env::temp_dir().join(format!("tabula-missing-{}.db", c.id));
    c.file = p.to_string_lossy().into();
    let err = Connection::connect(&c).await.err().expect("must fail");
    assert!(err.to_string().contains("not found"), "{err}");
    assert!(!p.exists(), "no empty database was created");
}

#[tokio::test]
async fn sqlite_end_to_end() {
    let cfg = cfg(Driver::Sqlite);
    let file = cfg.file.clone();
    sql_scenario(Driver::Sqlite).await;
    let _ = std::fs::remove_file(file);
}

#[tokio::test]
async fn postgres_end_to_end() {
    if it_enabled() {
        sql_scenario(Driver::Postgres).await;
    }
}

/// GitHub #13: choosing a schema makes unqualified names resolve in it,
/// on every pooled connection; `None` goes back to the default.
#[tokio::test]
async fn postgres_schema_switch() {
    if !it_enabled() {
        return;
    }
    let mut c = Connection::connect(&cfg(Driver::Postgres)).await.unwrap();
    c.execute(
        "drop schema if exists it_sales cascade; create schema it_sales; \
         create table it_sales.it_orders (id int); insert into it_sales.it_orders values (7)",
    )
    .await
    .unwrap();
    assert!(
        c.execute("select id from it_orders").await.is_err(),
        "not on the default path"
    );
    assert!(
        c.list_objects()
            .await
            .unwrap()
            .iter()
            .any(|o| o.schema.as_deref() == Some("it_sales"))
    );

    c.set_schema(Some("it_sales".into())).await.unwrap();
    assert_eq!(c.current_schema().as_deref(), Some("it_sales"));
    // Several runs so more than one pooled connection is exercised.
    for _ in 0..6 {
        let r = c.execute("select id from it_orders").await.unwrap();
        assert_eq!(r[0].rows[0][0].as_deref(), Some("7"));
    }
    // `public` stays on the path.
    assert!(
        c.execute("select count(*) from pg_catalog.pg_tables")
            .await
            .is_ok()
    );

    c.set_schema(None).await.unwrap();
    assert!(c.execute("select id from it_orders").await.is_err());
    // A SET in the editor doesn't leak into the shared pool (review #1):
    // later reads on any pooled connection still see the normal path.
    c.execute("set search_path to it_sales").await.unwrap();
    for _ in 0..6 {
        assert!(
            c.execute("select id from it_orders").await.is_err(),
            "search_path leaked"
        );
    }
    c.execute("drop schema it_sales cascade").await.unwrap();
}

#[tokio::test]
async fn mysql_end_to_end() {
    if it_enabled() {
        sql_scenario(Driver::MySql).await;
    }
}

/// The key editor: read each type, stage edits, TTL, rename, delete.
/// Database 9: each Redis test flushes its own database (5 end-to-end,
/// 6 insert-with-TTL, 7 the UI test, 8 screenshots).
#[tokio::test]
async fn redis_key_editor() {
    use redis_key::KeyEdit;
    if !it_enabled() {
        return;
    }
    let mut c = Connection::connect(&cfg(Driver::Redis)).await.unwrap();
    c.use_database("9").await.unwrap();
    c.execute(
        "FLUSHDB\nHSET ke:user name Ada age 36\nRPUSH ke:queue a b c d\nSADD ke:tags red blue\n\
         ZADD ke:board 10 ana 20 ben\nSET ke:json \"{\\\"a\\\":1}\"\nXADD ke:events * kind signup",
    )
    .await
    .unwrap();
    let hash = c.key_detail("ke:user").await.unwrap();
    assert_eq!(hash.kind, "hash");
    assert_eq!(hash.length, 2);
    assert!(
        hash.items.contains(&("name".into(), "Ada".into())),
        "{:?}",
        hash.items
    );
    assert_eq!(hash.ttl, None);
    let board = c.key_detail("ke:board").await.unwrap();
    assert_eq!(
        board.items,
        [("ana".into(), "10".into()), ("ben".into(), "20".into())]
    );
    let events = c.key_detail("ke:events").await.unwrap();
    assert_eq!(events.items.len(), 1);
    assert_eq!(events.items[0].1, "kind=signup");
    assert_eq!(
        c.key_detail("ke:json").await.unwrap().items[0].1,
        "{\"a\":1}"
    );

    c.apply_key_edits(
        "ke:user",
        &[
            KeyEdit::HashSet {
                field: "name".into(),
                value: "Ada L.".into(),
            },
            KeyEdit::HashDelete("age".into()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        c.key_detail("ke:user").await.unwrap().items,
        [("name".into(), "Ada L.".into())]
    );
    c.apply_key_edits(
        "ke:queue",
        &[
            KeyEdit::ListRemove(0),
            KeyEdit::ListSet {
                index: 1,
                value: "B".into(),
            },
            KeyEdit::ListRemove(3),
            KeyEdit::ListPush("e".into()),
        ],
    )
    .await
    .unwrap();
    let queue: Vec<String> = c
        .key_detail("ke:queue")
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|(_, value)| value)
        .collect();
    assert_eq!(queue, ["B", "c", "e"]);
    c.apply_key_edits(
        "ke:board",
        &[
            KeyEdit::ZsetAdd {
                member: "ana".into(),
                score: 30.0,
            },
            KeyEdit::ZsetRemove("ben".into()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        c.key_detail("ke:board").await.unwrap().items,
        [("ana".into(), "30".into())]
    );

    c.set_ttl("ke:tags", Some(100)).await.unwrap();
    let ttl = c.key_detail("ke:tags").await.unwrap().ttl.unwrap();
    assert!((90..=100).contains(&ttl), "{ttl}");
    c.set_ttl("ke:tags", None).await.unwrap();
    assert_eq!(c.key_detail("ke:tags").await.unwrap().ttl, None);
    assert!(
        c.rename_key("ke:tags", "ke:user").await.is_err(),
        "never overwrites"
    );
    c.rename_key("ke:tags", "ke:labels").await.unwrap();
    assert_eq!(c.key_detail("ke:labels").await.unwrap().kind, "set");
    c.delete_key("ke:labels").await.unwrap();
    assert_eq!(c.key_detail("ke:labels").await.unwrap().kind, "none");
    c.execute("FLUSHDB").await.unwrap();
}

#[tokio::test]
async fn redis_end_to_end() {
    if !it_enabled() {
        return;
    }
    let mut c = Connection::connect(&cfg(Driver::Redis)).await.unwrap();
    c.use_database("5").await.unwrap();
    c.execute("FLUSHDB\nSET it:greeting \"hello world\"\nHSET it:user:1 name Ada age 36\nRPUSH it:list a b c")
        .await
        .unwrap();
    let objects = c.list_objects().await.unwrap();
    let names: Vec<_> = objects.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["keys", "it:*"], "namespaces from key prefixes");
    // The namespace entry lists only its keys; a filter narrows within it.
    let mut ns_q = TableQuery {
        find: String::new(),
        object: objects[1].clone(),
        filters: vec![],
        order: None,
        limit: 50,
        offset: 0,
        match_any: false,
    };
    c.execute("SET other 1").await.unwrap();
    assert_eq!(c.fetch_table(&ns_q).await.unwrap().1, Some(3));
    ns_q.filters.push(Filter {
        column: "key".into(),
        op: FilterOp::Contains,
        value: "user".into(),
        value2: String::new(),
    });
    let (r, n) = c.fetch_table(&ns_q).await.unwrap();
    assert_eq!((n, r.rows[0][0].as_deref()), (Some(1), Some("it:user:1")));
    c.execute("DEL other").await.unwrap();
    let keys = objects[0].clone();
    let q = TableQuery {
        find: String::new(),
        object: keys.clone(),
        filters: vec![Filter {
            column: "key".into(),
            op: FilterOp::Contains,
            value: "it:".into(),
            value2: String::new(),
        }],
        order: None,
        limit: 50,
        offset: 0,
        match_any: false,
    };
    let (res, total) = c.fetch_table(&q).await.unwrap();
    assert_eq!(total, Some(3));
    let row = |k: &str, res: &QueryResult| {
        res.rows
            .iter()
            .find(|r| r[0].as_deref() == Some(k))
            .cloned()
            .unwrap()
    };
    assert_eq!(row("it:greeting", &res)[3].as_deref(), Some("hello world"));
    assert_eq!(row("it:user:1", &res)[1].as_deref(), Some("hash"));
    assert_eq!(row("it:list", &res)[3].as_deref(), Some(r#"["a","b","c"]"#));

    let st = c.structure(&keys).await.unwrap().columns;
    let changes = vec![
        Change::Update {
            key: kv(&[("key", Some("it:greeting"))]),
            set: kv(&[("value", Some("bye")), ("ttl", Some("300"))]),
        },
        Change::Insert {
            values: kv(&[
                ("key", Some("it:new")),
                ("type", None),
                ("ttl", None),
                ("value", Some("v")),
            ]),
        },
        Change::Delete {
            key: kv(&[("key", Some("it:list"))]),
        },
    ];
    c.commit(&keys, &st, &changes).await.unwrap();
    let (res, total) = c.fetch_table(&q).await.unwrap();
    assert_eq!(total, Some(3));
    let g = row("it:greeting", &res);
    assert_eq!(g[3].as_deref(), Some("bye"));
    assert!(g[2].as_deref().unwrap().parse::<i64>().unwrap() > 0);
    // Editing a hash value from the grid is refused rather than clobbering it.
    let bad = vec![Change::Update {
        key: kv(&[("key", Some("it:user:1"))]),
        set: kv(&[("value", Some("x"))]),
    }];
    assert!(c.commit(&keys, &st, &bad).await.is_err());
    // Rename + value + TTL in one commit (review #5): the new key gets the
    // new value and TTL, and the old key doesn't come back.
    let rename = vec![Change::Update {
        key: kv(&[("key", Some("it:greeting"))]),
        set: kv(&[
            ("key", Some("it:renamed")),
            ("value", Some("hi again")),
            ("ttl", Some("120")),
        ]),
    }];
    c.commit(&keys, &st, &rename).await.unwrap();
    let (res, _) = c.fetch_table(&q).await.unwrap();
    assert!(
        !res.rows
            .iter()
            .any(|r| r[0].as_deref() == Some("it:greeting")),
        "old key gone"
    );
    let renamed = row("it:renamed", &res);
    assert_eq!(renamed[3].as_deref(), Some("hi again"));
    assert!(renamed[2].as_deref().unwrap().parse::<i64>().unwrap() > 60);
    // Renaming onto an existing key, or adding one that exists, is refused
    // and changes nothing (review #5, #10).
    let clash = vec![Change::Update {
        key: kv(&[("key", Some("it:renamed"))]),
        set: kv(&[("key", Some("it:new")), ("value", Some("clobber"))]),
    }];
    assert!(c.commit(&keys, &st, &clash).await.is_err());
    let dup = vec![Change::Insert {
        values: kv(&[("key", Some("it:new")), ("value", Some("x"))]),
    }];
    assert!(c.commit(&keys, &st, &dup).await.is_err());
    let (res, _) = c.fetch_table(&q).await.unwrap();
    assert_eq!(
        row("it:new", &res)[3].as_deref(),
        Some("v"),
        "existing key untouched"
    );
    assert_eq!(
        row("it:renamed", &res)[3].as_deref(),
        Some("hi again"),
        "nothing applied"
    );

    let r = c.execute("HGETALL it:user:1").await.unwrap();
    assert_eq!(r[0].columns, ["field", "value"]);
    c.execute("FLUSHDB").await.unwrap();
}

#[tokio::test]
async fn mongo_end_to_end() {
    if !it_enabled() {
        return;
    }
    let c = Connection::connect(&cfg(Driver::Mongo)).await.unwrap();
    c.execute("db.people.drop()").await.ok();
    c.execute(r#"db.people.insertMany([{name: "Ada", age: 36, tags: ["math"]}, {name: "Linus", age: 54}, {name: "Grace", age: 85}])"#)
        .await
        .unwrap();
    let objects = c.list_objects().await.unwrap();
    let t = obj(&objects, "people");
    let st = c.structure(&t).await.unwrap();
    assert_eq!(st.columns[0].name, "_id");
    assert!(st.columns[0].is_pk);
    assert_eq!(
        st.columns
            .iter()
            .find(|c| c.name == "age")
            .unwrap()
            .data_type,
        "int"
    );

    let q = |filters: Vec<Filter>| TableQuery {
        find: String::new(),
        object: t.clone(),
        filters,
        order: Some(("age".into(), true)),
        limit: 50,
        offset: 0,
        match_any: false,
    };
    let (res, total) = c.fetch_table(&q(vec![])).await.unwrap();
    assert_eq!(total, Some(3));
    let name_ix = res.columns.iter().position(|c| c == "name").unwrap();
    assert_eq!(res.rows[0][name_ix].as_deref(), Some("Ada"));
    let ada_id = res.rows[0][0].clone();
    assert!(ada_id.as_deref().unwrap().starts_with("ObjectId("));

    // Grid edit: number stays a number, string column stays a string.
    let changes = vec![Change::Update {
        key: vec![("_id".into(), ada_id.clone())],
        set: kv(&[("age", Some("37")), ("name", Some("123"))]),
    }];
    c.commit(&t, &st.columns, &changes).await.unwrap();
    let (res, _) = c
        .fetch_table(&q(vec![Filter {
            column: "age".into(),
            op: FilterOp::Eq,
            value: "37".into(),
            value2: String::new(),
        }]))
        .await
        .unwrap();
    assert_eq!(res.rows.len(), 1);
    let r = c.execute(r#"db.people.find({name: "123"})"#).await.unwrap();
    assert_eq!(r[0].rows.len(), 1, "name stored as string");

    let (_, total) = c
        .fetch_table(&q(vec![Filter {
            column: "age".into(),
            op: FilterOp::Gt,
            value: "50".into(),
            value2: String::new(),
        }]))
        .await
        .unwrap();
    assert_eq!(total, Some(2));
    let (_, total) = c
        .fetch_table(&q(vec![Filter {
            column: "name".into(),
            op: FilterOp::Contains,
            value: "LIN".into(),
            value2: String::new(),
        }]))
        .await
        .unwrap();
    assert_eq!(total, Some(1));
    let mongo_filter = |column: &str, op: FilterOp, value: &str, value2: &str| Filter {
        column: column.into(),
        op,
        value: value.into(),
        value2: value2.into(),
    };
    for (filters, any, expected) in [
        (
            vec![mongo_filter("age", FilterOp::In, "54, 85", "")],
            false,
            2,
        ),
        (
            vec![mongo_filter("age", FilterOp::Between, "50", "60")],
            false,
            1,
        ),
        (
            vec![mongo_filter("name", FilterOp::StartsWith, "gr", "")],
            false,
            1,
        ),
        (
            vec![mongo_filter("name", FilterOp::Like, "L%s", "")],
            false,
            1,
        ),
        (
            vec![
                mongo_filter("name", FilterOp::Eq, "Grace", ""),
                mongo_filter("age", FilterOp::Lt, "40", ""),
            ],
            true,
            2,
        ),
    ] {
        let mut query = q(filters);
        query.match_any = any;
        let (_, total) = c.fetch_table(&query).await.unwrap();
        assert_eq!(total, Some(expected), "mongo {:?}", query.filters[0].op);
    }

    let r = c
        .execute(r#"db.people.aggregate([{$group: {_id: null, n: {$sum: 1}}}])"#)
        .await
        .unwrap();
    assert_eq!(r[0].rows[0][1].as_deref(), Some("3"));
    let r = c
        .execute(r#"db.people.find({}, {name: 1, _id: 0}).sort({age: -1}).limit(1)"#)
        .await
        .unwrap();
    assert_eq!(r[0].rows[0][0].as_deref(), Some("Grace"));
    let r = c.execute(r#"{ "ping": 1 }"#).await.unwrap();
    assert!(r[0].columns.contains(&"ok".to_string()));

    c.commit(
        &t,
        &st.columns,
        &[Change::Delete {
            key: vec![("_id".into(), ada_id.clone())],
        }],
    )
    .await
    .unwrap();
    // Deleting it again matches nothing: that's an error, not a silent
    // success (review #11). In a mixed commit, the error says what was saved.
    let again = [Change::Delete {
        key: vec![("_id".into(), ada_id.clone())],
    }];
    assert!(c.commit(&t, &st.columns, &again).await.is_err());
    let mixed = [
        Change::Insert {
            values: kv(&[("name", Some("Barbara")), ("age", Some("70"))]),
        },
        Change::Delete {
            key: vec![("_id".into(), ada_id)],
        },
    ];
    let err = c.commit(&t, &st.columns, &mixed).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("change 2 of 2 failed; the 1 before it were saved"),
        "{err:#}"
    );
    assert!(!c.commits_atomically());
    assert_eq!(
        c.fetch_table(&q(vec![])).await.unwrap().1,
        Some(3),
        "the insert was saved"
    );
    c.drop_object(&t).await.unwrap();
}

/// ⌘. stops a long statement on the server, not just in the app.
async fn cancel_scenario(driver: Driver, sleep_sql: &str, still_running: &str) {
    let c = Arc::new(Connection::connect(&cfg(driver)).await.expect("connect"));
    let cancel = Arc::new(Cancel::default());
    let run = spawn({
        let (c, cancel, sql) = (c.clone(), cancel.clone(), sleep_sql.to_string());
        async move { c.execute_cancellable(&sql, &cancel).await }
    });
    sleep(std::time::Duration::from_millis(500)).await;
    let start = Instant::now();
    cancel.cancel().await;
    let err = timeout(std::time::Duration::from_secs(5), run)
        .await
        .expect("cancel returns promptly")
        .unwrap()
        .unwrap_err();
    assert!(
        err.root_cause().to_string().starts_with(cancel::CANCELLED),
        "{err:#}"
    );
    assert!(start.elapsed().as_secs() < 5);
    // The server isn't still sleeping on our behalf.
    sleep(std::time::Duration::from_millis(300)).await;
    let r = c.execute(still_running).await.unwrap();
    assert_eq!(r[0].rows[0][0].as_deref(), Some("0"), "{driver:?}");
    // The pool still works afterwards.
    assert_eq!(
        c.execute("select 1").await.unwrap()[0].rows[0][0].as_deref(),
        Some("1")
    );
}

#[tokio::test]
async fn postgres_cancel() {
    if !it_enabled() {
        return;
    }
    cancel_scenario(
        Driver::Postgres,
        "select pg_sleep(30)",
        "select count(*) from pg_stat_activity where query = 'select pg_sleep(30)' and state = 'active' and pid <> pg_backend_pid()",
    )
    .await;
}

#[tokio::test]
async fn mysql_cancel() {
    if !it_enabled() {
        return;
    }
    cancel_scenario(
        Driver::MySql,
        "select sleep(30)",
        "select count(*) from information_schema.processlist where info = 'select sleep(30)'",
    )
    .await;
}

#[tokio::test]
async fn sqlite_cancel() {
    // A slow recursive query; SQLite just abandons it.
    let c = Connection::connect(&cfg(Driver::Sqlite)).await.unwrap();
    let cancel = Arc::new(Cancel::default());
    let c2 = cancel.clone();
    spawn(async move {
        sleep(std::time::Duration::from_millis(200)).await;
        c2.cancel().await;
    });
    let err = c
        .execute_cancellable(
            "with recursive n(x) as (select 1 union all select x + 1 from n where x < 100000000) select count(*) from n",
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(err.root_cause().to_string().starts_with(cancel::CANCELLED));
}

/// Structure view extras: foreign keys (with composite keys and actions)
/// and the table's CREATE statement.
async fn fk_scenario(driver: Driver) {
    let c = Connection::connect(&cfg(driver)).await.expect("connect");
    let int = if driver == Driver::MySql {
        "int"
    } else {
        "integer"
    };
    c.execute(&format!(
        "drop table if exists it_child; drop table if exists it_parent; \
         create table it_parent (a {int} not null, b {int} not null, name varchar(20), primary key (a, b)); \
         create table it_child (id {int} primary key, pa {int}, pb {int}, \
           constraint it_child_parent foreign key (pa, pb) references it_parent (a, b) on delete cascade)"
    ))
    .await
    .unwrap();
    let objects = c.list_objects().await.unwrap();
    let st = c.structure(&obj(&objects, "it_child")).await.unwrap();
    assert_eq!(st.foreign_keys.len(), 1, "{driver:?}");
    let fk = &st.foreign_keys[0];
    assert_eq!(fk.columns, ["pa", "pb"], "{driver:?}");
    assert_eq!(fk.ref_table, "it_parent");
    assert_eq!(fk.ref_columns, ["a", "b"], "{driver:?}");
    assert_eq!(fk.on_delete, "CASCADE", "{driver:?}");
    let ddl = st.ddl.to_lowercase();
    assert!(ddl.contains("create table"), "{driver:?}: {ddl}");
    assert!(ddl.contains("foreign key"), "{driver:?}: {ddl}");
    assert!(ddl.contains("it_parent"), "{driver:?}: {ddl}");
    // The parent lists the child's key, reversed.
    let parent = c.structure(&obj(&objects, "it_parent")).await.unwrap();
    assert_eq!(
        parent.referenced_by.len(),
        1,
        "{driver:?}: {:?}",
        parent.warnings
    );
    let inbound = &parent.referenced_by[0];
    assert_eq!(inbound.ref_table, "it_child", "{driver:?}");
    assert_eq!(inbound.columns, ["a", "b"], "{driver:?}");
    assert_eq!(inbound.ref_columns, ["pa", "pb"], "{driver:?}");
    c.execute("drop table it_child; drop table it_parent")
        .await
        .unwrap();
}

#[tokio::test]
async fn sqlite_foreign_keys() {
    fk_scenario(Driver::Sqlite).await;
}

#[tokio::test]
async fn postgres_foreign_keys() {
    if it_enabled() {
        fk_scenario(Driver::Postgres).await;
    }
}

#[tokio::test]
async fn mysql_foreign_keys() {
    if it_enabled() {
        fk_scenario(Driver::MySql).await;
    }
}

/// Enum columns list their values in order, and a picked value (or NULL)
/// commits.
async fn enum_scenario(driver: Driver) {
    let c = Connection::connect(&cfg(driver)).await.expect("connect");
    let setup = if driver == Driver::Postgres {
        "drop table if exists it_enum; drop type if exists it_mood; \
         create type it_mood as enum ('sad', 'it''s ok', 'happy'); \
         create table it_enum (id int primary key, mood it_mood, label text); \
         insert into it_enum values (1, 'sad', 'x')"
    } else {
        "drop table if exists it_enum; \
         create table it_enum (id int primary key, mood enum('sad', 'it''s ok', 'happy'), label text); \
         insert into it_enum values (1, 'sad', 'x')"
    };
    c.execute(setup).await.unwrap();
    let objects = c.list_objects().await.unwrap();
    let t = obj(&objects, "it_enum");
    let st = c.structure(&t).await.unwrap();
    let values = |name: &str| {
        st.columns
            .iter()
            .find(|c| c.name == name)
            .unwrap()
            .enum_values
            .clone()
    };
    assert_eq!(values("mood"), ["sad", "it's ok", "happy"], "{driver:?}");
    assert!(values("label").is_empty(), "{driver:?}");
    assert!(values("id").is_empty(), "{driver:?}");
    for v in [Some("it's ok"), None] {
        let changes = [Change::Update {
            key: kv(&[("id", Some("1"))]),
            set: kv(&[("mood", v)]),
        }];
        c.commit(&t, &st.columns, &changes).await.expect("commit");
        let r = c.execute("select mood from it_enum").await.unwrap();
        assert_eq!(r[0].rows[0][0].as_deref(), v, "{driver:?}");
    }
    let cleanup = if driver == Driver::Postgres {
        "drop table it_enum; drop type it_mood"
    } else {
        "drop table it_enum"
    };
    c.execute(cleanup).await.unwrap();
}

#[tokio::test]
async fn postgres_enum_values() {
    if it_enabled() {
        enum_scenario(Driver::Postgres).await;
    }
}

#[tokio::test]
async fn mysql_enum_values() {
    if it_enabled() {
        enum_scenario(Driver::MySql).await;
    }
}

/// `CHECK (col IN (…))` columns list their values like enums, as each
/// server spells the constraint back.
async fn check_in_scenario(driver: Driver) {
    let cfg = cfg(driver);
    let c = Connection::connect(&cfg).await.expect("connect");
    let ty = if driver == Driver::MySql {
        "varchar(20)"
    } else {
        "text"
    };
    c.execute(&format!(
        "drop table if exists it_check; \
         create table it_check (id int primary key, \
           state {ty} check (state in ('new', 'it''s done')), n int check (n > 0))"
    ))
    .await
    .unwrap();
    let objects = c.list_objects().await.unwrap();
    let st = c.structure(&obj(&objects, "it_check")).await.unwrap();
    let values = |name: &str| {
        st.columns
            .iter()
            .find(|c| c.name == name)
            .unwrap()
            .enum_values
            .clone()
    };
    assert_eq!(values("state"), ["new", "it's done"], "{driver:?}");
    assert!(values("n").is_empty(), "{driver:?}");
    c.execute("drop table it_check").await.unwrap();
    c.close().await;
    if driver == Driver::Sqlite {
        let _ = std::fs::remove_file(&cfg.file);
    }
}

#[tokio::test]
async fn check_in_values() {
    check_in_scenario(Driver::Sqlite).await;
    if it_enabled() {
        check_in_scenario(Driver::Postgres).await;
        check_in_scenario(Driver::MySql).await;
    }
}

/// The foreign-key picker's lookup: key prefix or label text, case-
/// insensitive, with LIKE wildcards taken literally.
async fn fk_matches_scenario(driver: Driver) {
    let cfg = cfg(driver);
    let c = Connection::connect(&cfg).await.expect("connect");
    c.execute(
        "drop table if exists it_fk_parent; \
         create table it_fk_parent (id int primary key, name varchar(40), email varchar(80)); \
         insert into it_fk_parent values (1, 'Priya Raman', 'priya@example.com'), \
           (2, 'Marcus Lee', 'marcus@example.com'), (12, 'Ana 100%_x', null)",
    )
    .await
    .unwrap();
    let objects = c.list_objects().await.unwrap();
    let t = obj(&objects, "it_fk_parent");
    let labels = ["name".to_string(), "email".to_string()];
    let keys = |m: Vec<(String, String)>| m.into_iter().map(|(k, _)| k).collect::<Vec<_>>();
    let all = c.fk_matches(&t, "id", &labels, "", 5).await.unwrap();
    assert_eq!(keys(all.clone()), ["1", "2", "12"], "{driver:?}");
    assert_eq!(all[0].1, "Priya Raman · priya@example.com", "{driver:?}");
    assert_eq!(
        keys(c.fk_matches(&t, "id", &labels, "1", 5).await.unwrap()),
        ["1", "12"],
        "{driver:?}"
    );
    assert_eq!(
        keys(c.fk_matches(&t, "id", &labels, "LEE", 5).await.unwrap()),
        ["2"],
        "{driver:?}"
    );
    // % and _ are just characters here.
    assert_eq!(
        keys(c.fk_matches(&t, "id", &labels, "%_", 5).await.unwrap()),
        ["12"],
        "{driver:?}"
    );
    assert_eq!(
        keys(c.fk_matches(&t, "id", &labels, "", 1).await.unwrap()),
        ["1"]
    );
    c.execute("drop table it_fk_parent").await.unwrap();
    c.close().await;
    if driver == Driver::Sqlite {
        let _ = std::fs::remove_file(&cfg.file);
    }
}

#[tokio::test]
async fn fk_matches() {
    fk_matches_scenario(Driver::Sqlite).await;
    if it_enabled() {
        fk_matches_scenario(Driver::Postgres).await;
        fk_matches_scenario(Driver::MySql).await;
    }
}

/// Postgres through an SSH tunnel (in-process SSH server → the test container).
#[tokio::test]
async fn postgres_over_ssh() {
    if !it_enabled() {
        return;
    }
    let (ssh_port, _) = ssh::test_server::start(None).await;
    let mut c = cfg(Driver::Postgres);
    c.ssh = Some(ssh::SshConfig {
        host: "127.0.0.1".into(),
        port: ssh_port,
        user: ssh::test_server::USER.into(),
        auth: ssh::SshAuth::Password,
        key_path: String::new(),
        secret: ssh::test_server::PASSWORD.into(),
    });
    let err = Connection::connect(&c)
        .await
        .err()
        .expect("host not trusted yet");
    err.downcast_ref::<ssh::UnknownHostKey>()
        .unwrap()
        .trust()
        .unwrap();
    let conn = Connection::connect(&c).await.expect("connect over ssh");
    let r = conn.execute("select 40 + 2").await.unwrap();
    assert_eq!(r[0].rows[0][0].as_deref(), Some("42"));
    // The pool opens several connections through the same tunnel.
    let (a, b) = join!(conn.execute("select 1"), conn.execute("select 2"));
    assert!(a.is_ok() && b.is_ok());
    conn.close().await;
}

/// Process list: another session's slow statement shows up and can be
/// cancelled from here.
async fn process_scenario(driver: Driver, sleep_sql: &'static str, query_col: &str) {
    let busy = Arc::new(Connection::connect(&cfg(driver)).await.unwrap());
    let admin = Connection::connect(&cfg(driver)).await.unwrap();
    let run = spawn({
        let busy = busy.clone();
        async move { busy.execute(sleep_sql).await }
    });
    sleep(std::time::Duration::from_millis(500)).await;
    let list = admin.processes().await.unwrap();
    let qc = list.columns.iter().position(|c| c == query_col).unwrap();
    let row = list
        .rows
        .iter()
        .find(|r| r[qc].as_deref() == Some(sleep_sql))
        .unwrap_or_else(|| panic!("{driver:?}: sleeping session listed in {:?}", list.rows));
    let id = row[0].clone().unwrap();
    admin.kill(&id, false).await.unwrap();
    let res = timeout(std::time::Duration::from_secs(5), run)
        .await
        .expect("cancelled promptly")
        .unwrap();
    // Postgres reports the cancel as an error; MySQL's SLEEP returns 1.
    match driver {
        Driver::Postgres => assert!(res.is_err()),
        _ => assert_eq!(res.unwrap()[0].rows[0][0].as_deref(), Some("1")),
    }
    // Our own session is never listed.
    let me = admin.processes().await.unwrap();
    assert!(me.rows.iter().all(|r| r[qc].as_deref() != Some(sleep_sql)));
}

#[tokio::test]
async fn postgres_processes() {
    if it_enabled() {
        process_scenario(Driver::Postgres, "select pg_sleep(20)", "query").await;
    }
}

#[tokio::test]
async fn mysql_processes() {
    if it_enabled() {
        process_scenario(Driver::MySql, "select sleep(20)", "query").await;
    }
}

#[tokio::test]
async fn redis_clients() {
    if !it_enabled() {
        return;
    }
    let other = Connection::connect(&cfg(Driver::Redis)).await.unwrap();
    other
        .execute("CLIENT SETNAME tabula-it-other")
        .await
        .unwrap();
    let admin = Connection::connect(&cfg(Driver::Redis)).await.unwrap();
    let list = admin.processes().await.unwrap();
    let row = list
        .rows
        .iter()
        .find(|r| r[2].as_deref() == Some("tabula-it-other"))
        .expect("named client listed");
    admin.kill(row[0].as_deref().unwrap(), true).await.unwrap();
    let after = admin.processes().await.unwrap();
    assert!(
        after
            .rows
            .iter()
            .all(|r| r[2].as_deref() != Some("tabula-it-other"))
    );
}

/// Read-only safe mode is enforced by the server too, so a write the
/// editor's guess misses still fails.
async fn read_only_scenario(driver: Driver) {
    let mut c = cfg(driver);
    let setup = Connection::connect(&c).await.unwrap();
    setup
        .execute("drop table if exists it_ro; create table it_ro (id int)")
        .await
        .unwrap();
    c.safe_mode = SafeMode::ReadOnly;
    let ro = Connection::connect(&c).await.unwrap();
    assert!(ro.execute("select count(*) from it_ro").await.is_ok());
    // Looks like a read to a naive parser; the server still refuses.
    let sneaky = "select 1; insert into it_ro values (1)";
    assert!(ro.execute(sneaky).await.is_err(), "{driver:?}");
    assert!(
        ro.execute("insert into it_ro values (2)").await.is_err(),
        "{driver:?}"
    );
    let n = setup.execute("select count(*) from it_ro").await.unwrap();
    assert_eq!(n[0].rows[0][0].as_deref(), Some("0"));
    setup.execute("drop table it_ro").await.unwrap();
}

#[tokio::test]
async fn sqlite_read_only() {
    let mut c = cfg(Driver::Sqlite);
    let setup = Connection::connect(&c).await.unwrap();
    setup.execute("create table it_ro (id int)").await.unwrap();
    c.safe_mode = SafeMode::ReadOnly;
    let ro = Connection::connect(&c).await.unwrap();
    assert!(ro.execute("insert into it_ro values (1)").await.is_err());
    assert!(ro.execute("select count(*) from it_ro").await.is_ok());
}

#[tokio::test]
async fn postgres_read_only() {
    if it_enabled() {
        read_only_scenario(Driver::Postgres).await;
    }
}

#[tokio::test]
async fn mysql_read_only() {
    if it_enabled() {
        read_only_scenario(Driver::MySql).await;
    }
}

/// Table editor DDL runs as generated on each server.
async fn ddl_scenario(driver: Driver) {
    use ddl::{ColChange, ColDef};
    let c = Connection::connect(&cfg(driver)).await.unwrap();
    c.execute("drop table if exists it_ddl").await.unwrap();
    let int = if driver == Driver::MySql {
        "int"
    } else {
        "integer"
    };
    let text = if driver == Driver::MySql {
        "varchar(50)"
    } else {
        "text"
    };
    let col = |n: &str, t: &str, nullable, d: Option<&str>, pk| ColDef {
        name: n.into(),
        ty: t.into(),
        nullable,
        default: d.map(String::from),
        pk,
        comment: None,
    };
    let create = ddl::create_table(
        driver,
        None,
        "it_ddl",
        &[
            col("id", int, false, None, true),
            col("name", text, true, None, false),
        ],
    )
    .unwrap();
    c.execute(&ddl::script(driver, &[create])).await.unwrap();
    let mut changes = vec![
        ColChange::Add(col("age", int, true, Some("0"), false)),
        ColChange::Alter {
            from: col("name", text, true, None, false),
            to: col("full_name", text, true, None, false),
        },
    ];
    if driver != Driver::Sqlite {
        changes.push(ColChange::Alter {
            from: col("full_name", text, true, None, false),
            to: col("full_name", text, false, Some("'x'"), false),
        });
    }
    let stmts = ddl::alter_table(driver, None, "it_ddl", &changes).unwrap();
    c.execute(&ddl::script(driver, &stmts)).await.unwrap();
    let objects = c.list_objects().await.unwrap();
    let st = c.structure(&obj(&objects, "it_ddl")).await.unwrap();
    let names: Vec<_> = st.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "full_name", "age"], "{driver:?}");
    if driver != Driver::Sqlite {
        assert!(!st.columns[1].nullable, "{driver:?}");
    }
    // A failing script leaves nothing half-applied where DDL is transactional.
    if driver != Driver::MySql {
        let bad = ddl::script(
            driver,
            &[
                "ALTER TABLE it_ddl ADD COLUMN extra int;".into(),
                "ALTER TABLE it_ddl DROP COLUMN no_such_column;".into(),
            ],
        );
        assert!(c.execute(&bad).await.is_err());
        let st = c.structure(&obj(&objects, "it_ddl")).await.unwrap();
        assert!(
            st.columns.iter().all(|c| c.name != "extra"),
            "{driver:?} rolled back"
        );
    }
    // Column comments round-trip (SQLite has none).
    if driver != Driver::Sqlite {
        let st = c.structure(&obj(&objects, "it_ddl")).await.unwrap();
        let age = ColDef::from_info(
            st.columns
                .iter()
                .find(|column| column.name == "age")
                .unwrap(),
        );
        let commented = ColDef {
            comment: Some("Years, rounded down".into()),
            ..age.clone()
        };
        let stmts = ddl::alter_table(
            driver,
            None,
            "it_ddl",
            &[ColChange::Alter {
                from: age,
                to: commented,
            }],
        )
        .unwrap();
        c.execute(&ddl::script(driver, &stmts)).await.unwrap();
        let st = c.structure(&obj(&objects, "it_ddl")).await.unwrap();
        let age = st
            .columns
            .iter()
            .find(|column| column.name == "age")
            .unwrap();
        assert_eq!(
            age.comment.as_deref(),
            Some("Years, rounded down"),
            "{driver:?}"
        );
        assert_eq!(
            age.default.as_deref().map(str::trim),
            Some("0"),
            "{driver:?}: the default survives"
        );
    }
    // + Index and Drop Index…, read back through the index list.
    let table = obj(&objects, "it_ddl");
    let index_names = |structure: &Structure| -> Vec<String> {
        ddl::index_entries(driver, &structure.indexes)
            .into_iter()
            .map(|entry| entry.name)
            .collect()
    };
    let create_index = ddl::create_index(
        driver,
        table.schema.as_deref(),
        "it_ddl",
        "it_ddl_age_idx",
        &["age".to_string(), "id".to_string()],
        true,
    )
    .unwrap();
    c.execute(&create_index).await.unwrap();
    let st = c.structure(&table).await.unwrap();
    assert!(
        index_names(&st).contains(&"it_ddl_age_idx".to_string()),
        "{driver:?}: {:?}",
        index_names(&st)
    );
    let drop_index = ddl::drop_index(driver, table.schema.as_deref(), "it_ddl", "it_ddl_age_idx");
    c.execute(&drop_index).await.unwrap();
    let st = c.structure(&table).await.unwrap();
    assert!(
        !index_names(&st).contains(&"it_ddl_age_idx".to_string()),
        "{driver:?}"
    );
    c.execute("drop table it_ddl").await.unwrap();
}

/// ⌘K's sizes and counts: Postgres sizes, MySQL sizes and table counts,
/// Redis key counts, MongoDB sizes.
#[tokio::test]
async fn database_stats_per_server() {
    if !it_enabled() {
        return;
    }
    let postgres = Connection::connect(&cfg(Driver::Postgres)).await.unwrap();
    let stats = postgres.database_stats().await.unwrap();
    let main = stats.iter().find(|stats| stats.name == "postgres").unwrap();
    assert!(main.size_bytes.unwrap() > 0);

    let mysql = Connection::connect(&cfg(Driver::MySql)).await.unwrap();
    let stats = mysql.database_stats().await.unwrap();
    let system = stats.iter().find(|stats| stats.name == "mysql").unwrap();
    assert!(system.objects.unwrap() > 0, "{stats:?}");
    mysql.execute("drop table if exists it_estimate; create table it_estimate (id int primary key); insert into it_estimate values (1), (2); analyze table it_estimate").await.unwrap();
    let estimates = mysql.approx_row_counts().await.unwrap();
    assert!(
        estimates.iter().any(|(_, name, _)| name == "it_estimate"),
        "{estimates:?}"
    );
    mysql.execute("drop table it_estimate").await.unwrap();

    let mut redis = Connection::connect(&cfg(Driver::Redis)).await.unwrap();
    redis.use_database("10").await.unwrap();
    redis
        .execute("FLUSHDB\nSET stats:a 1\nSET stats:b 2")
        .await
        .unwrap();
    let stats = redis.database_stats().await.unwrap();
    let ten = stats.iter().find(|stats| stats.name == "10").unwrap();
    assert_eq!(ten.objects, Some(2));
    redis.execute("FLUSHDB").await.unwrap();

    let mongo = Connection::connect(&cfg(Driver::Mongo)).await.unwrap();
    mongo
        .execute("db.stats_probe.insertOne({a: 1})")
        .await
        .unwrap();
    let estimates = mongo.approx_row_counts().await.unwrap();
    assert!(
        estimates
            .iter()
            .any(|(_, name, rows)| name == "stats_probe" && *rows >= 1)
    );
    let stats = mongo.database_stats().await.unwrap();
    assert!(
        stats
            .iter()
            .any(|stats| stats.name == "tabula_it" && stats.size_bytes.is_some())
    );
}

#[tokio::test]
async fn sqlite_ddl() {
    ddl_scenario(Driver::Sqlite).await;
}

#[tokio::test]
async fn postgres_ddl() {
    if it_enabled() {
        ddl_scenario(Driver::Postgres).await;
    }
}

#[tokio::test]
async fn mysql_ddl() {
    if it_enabled() {
        ddl_scenario(Driver::MySql).await;
    }
}

/// Backup with pg_dump, then restore with psql into another database.
/// Uses its own throwaway databases, so parallel tests can't interfere.
#[tokio::test]
async fn postgres_backup_restore() {
    if !it_enabled() || backup::find_tool(&["pg_dump"]).is_none() {
        return;
    }
    let admin = Connection::connect(&cfg(Driver::Postgres)).await.unwrap();
    let tag = Uuid::new_v4().simple().to_string();
    // Named to sort before "postgres", so the ⌘K picker test (which steps
    // from postgres to the next database) isn't thrown off meanwhile.
    let (src, dst) = (
        format!("_tabula_bk_{}", &tag[..8]),
        format!("_tabula_rs_{}", &tag[..8]),
    );
    for db in [&src, &dst] {
        admin
            .execute(&format!("create database {db}"))
            .await
            .unwrap();
    }
    let at = |db: &str| {
        let mut c = cfg(Driver::Postgres);
        c.database = db.into();
        c
    };
    let c = Connection::connect(&at(&src)).await.unwrap();
    c.execute("create table it_backup (id int primary key, note text); insert into it_backup values (1, 'it''s'), (2, null)")
        .await
        .unwrap();
    let (host, port) = c.endpoint();
    let t = backup::Target {
        cfg: at(&src),
        host,
        port,
        database: src.clone(),
    };
    let file = std::env::temp_dir().join(format!("tabula-it-{tag}.sql"));
    backup::dump(&t, backup::Scope::Everything, &file)
        .await
        .unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("CREATE TABLE public.it_backup"),
        "schema dumped"
    );

    let t2 = backup::Target {
        cfg: at(&dst),
        database: dst.clone(),
        ..t.clone()
    };
    backup::restore(&t2, &file).await.unwrap();
    let o = Connection::connect(&at(&dst)).await.unwrap();
    let r = o
        .execute("select note from it_backup order by id")
        .await
        .unwrap();
    assert_eq!(r[0].rows, vec![vec![Some("it's".to_string())], vec![None]]);
    // A second restore fails (table exists) and, being one transaction, changes nothing.
    assert!(backup::restore(&t2, &file).await.is_err());
    let r = o.execute("select count(*) from it_backup").await.unwrap();
    assert_eq!(r[0].rows[0][0].as_deref(), Some("2"));
    c.close().await;
    o.close().await;
    for db in [&src, &dst] {
        admin
            .execute(&format!("drop database {db} with (force)"))
            .await
            .unwrap();
    }
    let _ = std::fs::remove_file(file);
}

#[tokio::test]
async fn sqlite_backup() {
    let c = cfg(Driver::Sqlite);
    let conn = Connection::connect(&c).await.unwrap();
    conn.execute("create table t (a int); insert into t values (7)")
        .await
        .unwrap();
    let t = backup::Target {
        cfg: c.clone(),
        host: String::new(),
        port: 0,
        database: "main".into(),
    };
    let file = std::env::temp_dir().join(format!("tabula-it-{}.sqlite", Uuid::new_v4()));
    backup::dump(&t, backup::Scope::Everything, &file)
        .await
        .unwrap();
    let mut copy = ConnectionConfig::new(Driver::Sqlite);
    copy.file = file.to_string_lossy().into();
    let b = Connection::connect(&copy).await.unwrap();
    assert_eq!(
        b.execute("select a from t").await.unwrap()[0].rows[0][0].as_deref(),
        Some("7")
    );
    let _ = std::fs::remove_file(file);
}

/// Real servers' plans parse into a tree.
async fn explain_scenario(driver: Driver) {
    let c = Connection::connect(&cfg(driver)).await.unwrap();
    let int = if driver == Driver::MySql {
        "int"
    } else {
        "integer"
    };
    c.execute(&format!(
        "drop table if exists it_ex; create table it_ex (id {int} primary key, v int); insert into it_ex values (1, 1), (2, 2)"
    ))
    .await
    .unwrap();
    for analyze in [false, true] {
        let sql = explain::explain_sql(
            driver,
            "select * from it_ex where id = 1 or v > 1;",
            analyze,
        )
        .unwrap();
        let r = c.execute(&sql).await.unwrap();
        let plan =
            explain::parse(driver, &r[0]).unwrap_or_else(|e| panic!("{driver:?}: {e:#} {r:?}"));
        assert!(!plan.flatten().is_empty());
        let text = format!("{plan:?}");
        assert!(text.contains("it_ex"), "{driver:?}: {text}");
        if analyze && driver != Driver::Sqlite {
            assert!(plan.actual_ms.is_some(), "{driver:?}: {text}");
        }
    }
    c.execute("drop table it_ex").await.unwrap();
}

#[tokio::test]
async fn sqlite_explain() {
    explain_scenario(Driver::Sqlite).await;
}

#[tokio::test]
async fn postgres_explain() {
    if it_enabled() {
        explain_scenario(Driver::Postgres).await;
    }
}

#[tokio::test]
async fn mysql_explain() {
    if it_enabled() {
        explain_scenario(Driver::MySql).await;
    }
}

/// EXPLAIN ANALYZE of a write measures it without keeping it.
async fn analyze_rolls_back(driver: Driver) {
    let c = Connection::connect(&cfg(driver)).await.unwrap();
    c.execute("drop table if exists it_an; create table it_an (id int primary key); insert into it_an values (1), (2)")
        .await
        .unwrap();
    let script = explain::explain_script(driver, "delete from it_an", true).unwrap();
    let rs = c.execute(&script).await.unwrap();
    let r = rs
        .iter()
        .find(|r| r.statement.to_uppercase().starts_with("EXPLAIN"))
        .unwrap();
    match driver {
        // MySQL only analyzes reads, and says so.
        Driver::MySql => {
            let e = explain::parse(driver, r).unwrap_err();
            assert!(format!("{e:#}").contains("only EXPLAIN ANALYZE"));
        }
        _ => assert!(
            explain::parse(driver, r).unwrap().actual_ms.is_some(),
            "{driver:?}"
        ),
    }
    let n = c.execute("select count(*) from it_an").await.unwrap();
    assert_eq!(
        n[0].rows[0][0].as_deref(),
        Some("2"),
        "{driver:?}: delete rolled back"
    );
    c.execute("drop table it_an").await.unwrap();
}

#[tokio::test]
async fn postgres_analyze_rolls_back() {
    if it_enabled() {
        analyze_rolls_back(Driver::Postgres).await;
    }
}

#[tokio::test]
async fn mysql_analyze_rolls_back() {
    if it_enabled() {
        analyze_rolls_back(Driver::MySql).await;
    }
}

/// Sidebar object operations: rename, duplicate (with data), maintenance.
async fn object_ops(driver: Driver) {
    let c = Connection::connect(&cfg(driver)).await.unwrap();
    c.execute("drop table if exists it_ops_b; drop table if exists it_ops_c; drop table if exists it_ops_a")
        .await
        .unwrap();
    c.execute("create table it_ops_a (id int primary key, v text not null); insert into it_ops_a values (1, 'x')")
        .await
        .unwrap();
    let objs = c.list_objects().await.unwrap();
    let a = obj(&objs, "it_ops_a");
    c.rename_object(&a, "it_ops_b").await.unwrap();
    let objs = c.list_objects().await.unwrap();
    let b = obj(&objs, "it_ops_b");
    assert!(objs.iter().all(|o| o.name != "it_ops_a"));
    c.duplicate_table(&b, "it_ops_c", true).await.unwrap();
    let objs = c.list_objects().await.unwrap();
    let cc = obj(&objs, "it_ops_c");
    let st = c.structure(&cc).await.unwrap();
    assert!(st.columns.iter().any(|c| c.is_pk), "{driver:?}: key copied");
    assert!(!st.columns[1].nullable, "{driver:?}: NOT NULL copied");
    let r = c.execute("select v from it_ops_c").await.unwrap();
    assert_eq!(r[0].rows[0][0].as_deref(), Some("x"));
    c.maintain(&cc).await.unwrap();
    c.execute("drop table it_ops_b; drop table it_ops_c")
        .await
        .unwrap();
}

#[tokio::test]
async fn sqlite_object_ops() {
    object_ops(Driver::Sqlite).await;
}

#[tokio::test]
async fn postgres_object_ops() {
    if it_enabled() {
        object_ops(Driver::Postgres).await;
    }
}

#[tokio::test]
async fn mysql_object_ops() {
    if it_enabled() {
        object_ops(Driver::MySql).await;
    }
}

#[tokio::test]
async fn postgres_create_drop_database() {
    if !it_enabled() {
        return;
    }
    let c = Connection::connect(&cfg(Driver::Postgres)).await.unwrap();
    let name = format!("_tabula_db_{}", &Uuid::new_v4().simple().to_string()[..8]);
    c.create_database(&name).await.unwrap();
    assert!(c.databases().await.unwrap().contains(&name));
    assert!(
        c.drop_database("postgres").await.is_err(),
        "not the current one"
    );
    c.drop_database(&name).await.unwrap();
    assert!(!c.databases().await.unwrap().contains(&name));
}

/// Connecting to nothing fails fast with the address and the real reason,
/// not "pool timed out".
#[tokio::test]
async fn unreachable_server_says_why() {
    for d in [
        Driver::Postgres,
        Driver::MySql,
        Driver::Redis,
        Driver::Mongo,
    ] {
        let mut c = cfg(d);
        c.port = 1; // nothing listens there
        let start = Instant::now();
        let e = Connection::connect(&c).await.err().expect("fails");
        let msg = format!("{e:#}");
        assert!(msg.contains("127.0.0.1:1"), "{d:?}: {msg}");
        assert!(!msg.contains("pool timed out"), "{d:?}: {msg}");
        assert!(
            start.elapsed().as_secs() < 20,
            "{d:?} took {:?}",
            start.elapsed()
        );
    }
}

/// A server that stops answering: background reads give up with a clear
/// message instead of hanging, and work again once it's back.
#[tokio::test]
async fn frozen_server_times_out_and_recovers() {
    if !it_enabled() || std::env::var("TABULA_IT_PAUSE").is_err() {
        return;
    }
    let c = Connection::connect(&cfg(Driver::Redis)).await.unwrap();
    let keys = c.list_objects().await.unwrap().remove(0);
    let q = TableQuery {
        find: String::new(),
        object: keys,
        filters: vec![],
        order: None,
        limit: 5,
        offset: 0,
        match_any: false,
    };
    std::process::Command::new("docker")
        .args(["pause", "tabula-test-redis"])
        .status()
        .unwrap();
    let e = c.fetch_table(&q).await.unwrap_err();
    std::process::Command::new("docker")
        .args(["unpause", "tabula-test-redis"])
        .status()
        .unwrap();
    assert!(format!("{e:#}").contains("Lost the connection"), "{e:#}");
    assert!(c.fetch_table(&q).await.is_ok());
}

/// A new key with a TTL gets it; a key someone else already took is
/// reported and keeps its own TTL / value.
#[tokio::test]
async fn redis_insert_with_ttl() {
    if !it_enabled() {
        return;
    }
    let mut c = Connection::connect(&cfg(Driver::Redis)).await.unwrap();
    c.use_database("6").await.unwrap();
    c.execute("FLUSHDB").await.unwrap();
    let keys = c.list_objects().await.unwrap().remove(0);
    let st = c.structure(&keys).await.unwrap().columns;
    let ins = |k: &str| Change::Insert {
        values: kv(&[
            ("key", Some(k)),
            ("ttl", Some("100")),
            ("value", Some("ours")),
        ]),
    };
    c.commit(&keys, &st, &[ins("it:fresh")]).await.unwrap();
    let r = c.execute("TTL it:fresh").await.unwrap();
    let shown = format!("{:?}", r[0].rows);
    assert!(shown.contains("100") || shown.contains("99"), "{shown}");
    // Someone else's key: refused, and left exactly as it was.
    c.execute("SET it:taken theirs").await.unwrap();
    let e = c.commit(&keys, &st, &[ins("it:taken")]).await.unwrap_err();
    assert!(format!("{e:#}").contains("already exists"), "{e:#}");
    let r = c.execute("TTL it:taken").await.unwrap();
    assert!(format!("{:?}", r[0].rows).contains("-1"), "no TTL added");
    c.execute("FLUSHDB").await.unwrap();
}

/// The status bar's "PostgreSQL 17.2": a plain version number per server.
#[tokio::test]
async fn server_versions() {
    let drivers: &[Driver] = if it_enabled() {
        &[
            Driver::Sqlite,
            Driver::Postgres,
            Driver::MySql,
            Driver::Redis,
            Driver::Mongo,
        ]
    } else {
        &[Driver::Sqlite]
    };
    for &d in drivers {
        let cfg = cfg(d);
        let c = Connection::connect(&cfg).await.unwrap();
        let v = c
            .server_version(d)
            .await
            .map_err(|e| format!("{d:?}: {e}"))
            .unwrap();
        assert!(
            v.chars().next().is_some_and(|c| c.is_ascii_digit()) && !v.contains(' '),
            "{d:?}: {v:?}"
        );
        c.close().await;
        if d == Driver::Sqlite {
            let _ = std::fs::remove_file(&cfg.file);
        }
    }
}
