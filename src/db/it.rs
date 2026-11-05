//! End-to-end checks against real servers. SQLite always runs; the others
//! run when TABULA_IT=1 and expect the throwaway containers on these ports:
//!   pg 55432 (postgres/test), mysql 53306 (root/test, db app),
//!   redis 56379, mongo 57017.

use super::*;

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
        object: obj.clone(),
        filters: vec![],
        order: None,
        limit: 1,
        offset: 0,
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
        object: t.clone(),
        filters,
        order,
        limit: 100,
        offset: 0,
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
            }],
            None,
        ))
        .await
        .unwrap();
    assert_eq!(total, Some(1));

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
    let keys = c.list_objects().await.unwrap().remove(0);
    let q = TableQuery {
        object: keys.clone(),
        filters: vec![Filter {
            column: "key".into(),
            op: FilterOp::Contains,
            value: "it:".into(),
        }],
        order: None,
        limit: 50,
        offset: 0,
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
        object: t.clone(),
        filters,
        order: Some(("age".into(), true)),
        limit: 50,
        offset: 0,
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
        }]))
        .await
        .unwrap();
    assert_eq!(total, Some(2));
    let (_, total) = c
        .fetch_table(&q(vec![Filter {
            column: "name".into(),
            op: FilterOp::Contains,
            value: "LIN".into(),
        }]))
        .await
        .unwrap();
    assert_eq!(total, Some(1));

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
