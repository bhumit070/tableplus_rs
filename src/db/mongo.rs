//! MongoDB: collections are shown as tables whose columns are the union of
//! top-level fields. The editor accepts mongosh-style calls
//! (`db.users.find({age: {$gt: 30}})`) or a raw command document.

use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use futures::TryStreamExt;
use mongodb::bson::{Bson, Document, doc};
use mongodb::{Client, Collection, Database};

use super::{
    Change, ColumnInfo, ConnectionConfig, DatabaseStats, DbObject, FilterOp, ObjectKind,
    QueryResult, Structure, TableQuery,
};
use anyhow::Error;
use json5::from_str;
use mongodb::options::ClientOptions;
use serde_json::Value;

pub struct MongoConn {
    client: Client,
    db: Database,
}

// ---- value conversion -----------------------------------------------------

pub fn render(b: &Bson) -> Option<String> {
    Some(match b {
        Bson::Null | Bson::Undefined => return None,
        Bson::String(s) => s.clone(),
        Bson::ObjectId(o) => format!("ObjectId(\"{o}\")"),
        Bson::DateTime(d) => format!(
            "ISODate(\"{}\")",
            d.try_to_rfc3339_string()
                .unwrap_or_else(|_| d.timestamp_millis().to_string())
        ),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(n) => n.to_string(),
        Bson::Boolean(v) => v.to_string(),
        other => other.clone().into_relaxed_extjson().to_string(),
    })
}

fn bson_type(b: &Bson) -> &'static str {
    match b {
        Bson::String(_) => "string",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Double(_) => "double",
        Bson::Boolean(_) => "bool",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Decimal128(_) => "decimal",
        Bson::Null => "null",
        _ => "mixed",
    }
}

/// Rewrites mongosh helpers (`ObjectId("…")`, `ISODate("…")`) to extended
/// JSON so JSON5 can parse them.
fn desugar(s: &str) -> String {
    let mut out = s.to_string();
    for (func, key) in [
        ("ObjectId", "$oid"),
        ("ISODate", "$date"),
        ("new Date", "$date"),
    ] {
        while let Some(i) = out.find(&format!("{func}(")) {
            let open = i + func.len() + 1;
            let Some(close) = out[open..].find(')').map(|c| open + c) else {
                break;
            };
            let inner = out[open..close].trim();
            out.replace_range(i..=close, &format!("{{\"{key}\": {inner}}}"));
        }
    }
    out
}

fn parse_json5(s: &str) -> Result<Bson> {
    let v: Value = from_str(&desugar(s)).map_err(|e| anyhow!("{e}"))?;
    Bson::try_from(v).map_err(|e| anyhow!("{e}"))
}

fn parse_doc(s: &str) -> Result<Document> {
    match parse_json5(s)? {
        Bson::Document(d) => Ok(d),
        _ => bail!("expected a document: {s}"),
    }
}

/// Grid text → BSON. String columns stay strings; everything else is parsed.
fn to_bson(v: &Option<String>, col_type: Option<&str>) -> Bson {
    match v {
        None => Bson::Null,
        Some(s) if col_type == Some("string") => Bson::String(s.clone()),
        Some(s) => parse_json5(s).unwrap_or_else(|_| Bson::String(s.clone())),
    }
}

fn docs_to_result(docs: Vec<Document>) -> QueryResult {
    let mut columns: Vec<String> = Vec::new();
    for d in &docs {
        for k in d.keys() {
            if !columns.contains(k) {
                if k == "_id" {
                    columns.insert(0, k.clone())
                } else {
                    columns.push(k.clone())
                }
            }
        }
    }
    let rows = docs
        .iter()
        .map(|d| columns.iter().map(|c| d.get(c).and_then(render)).collect())
        .collect();
    QueryResult {
        columns,
        rows,
        ..Default::default()
    }
}

/// Splits `a, b, c` at top level, then parses each with JSON5.
fn parse_args(s: &str) -> Result<Vec<Bson>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(vec![]);
    }
    let v: Value = from_str(&format!("[{}]", desugar(s))).map_err(|e| anyhow!("{e}"))?;
    match Bson::try_from(v).map_err(|e| anyhow!("{e}"))? {
        Bson::Array(a) => Ok(a),
        _ => unreachable!(),
    }
}

fn arg_doc(args: &[Bson], i: usize) -> Result<Document> {
    match args.get(i) {
        None => Ok(Document::new()),
        Some(Bson::Document(d)) => Ok(d.clone()),
        Some(_) => bail!("argument {} must be a document", i + 1),
    }
}

/// Finds the `(…)` of a call starting at `open` and returns (inside, rest).
fn take_call(s: &str, open: usize) -> Result<(&str, &str)> {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    for (i, c) in s[open..].char_indices() {
        let i = open + i;
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[' | '{') => depth += 1,
            (None, ')' | ']' | '}') => {
                depth -= 1;
                if depth == 0 {
                    return Ok((&s[open + 1..i], &s[i + 1..]));
                }
            }
            _ => {}
        }
    }
    bail!("unbalanced parentheses")
}

impl MongoConn {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let uri = if cfg.host.starts_with("mongodb") {
            // A pasted URI is saved without its password (db/url.rs).
            super::url::with_password(&cfg.host, &cfg.password)
        } else {
            let auth = if cfg.user.is_empty() {
                String::new()
            } else {
                format!("{}:{}@", urlencode(&cfg.user), urlencode(&cfg.password))
            };
            let tls = if cfg.ssl { "&tls=true" } else { "" };
            // Through a tunnel, talk to that one server: replica-set
            // discovery would chase member addresses the tunnel doesn't reach.
            let direct = if cfg.ssh.is_some() {
                "&directConnection=true"
            } else {
                ""
            };
            format!(
                "mongodb://{auth}{}:{}/?authSource=admin{tls}{direct}",
                cfg.host, cfg.port
            )
        };
        let mut opts = ClientOptions::parse(&uri)
            .await
            .map_err(|e| Error::new(e).context("reading the MongoDB address"))?;
        // Fail in seconds, not the driver's default 30 s, unless the URI says otherwise.
        opts.server_selection_timeout
            .get_or_insert(std::time::Duration::from_secs(8));
        opts.connect_timeout
            .get_or_insert(std::time::Duration::from_secs(10));
        let at = opts
            .hosts
            .iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let client = Client::with_options(opts)?;
        client
            .database("admin")
            .run_command(doc! {"ping": 1})
            .await
            .map_err(|e| Error::new(e).context(format!("couldn't connect to {at}")))?;
        let name = if !cfg.database.is_empty() {
            cfg.database.clone()
        } else {
            client
                .default_database()
                .map(|d| d.name().to_string())
                .or(client
                    .list_database_names()
                    .await?
                    .into_iter()
                    .find(|n| !["admin", "local", "config"].contains(&n.as_str())))
                .unwrap_or_else(|| "test".into())
        };
        let db = client.database(&name);
        Ok(Self { client, db })
    }

    fn coll(&self, name: &str) -> Collection<Document> {
        self.db.collection(name)
    }

    pub fn current_database(&self) -> String {
        self.db.name().to_string()
    }

    pub async fn databases(&self) -> Result<Vec<String>> {
        Ok(self.client.list_database_names().await?)
    }

    /// Per collection, from its metadata (estimatedDocumentCount).
    pub async fn approx_row_counts(&self) -> Result<Vec<(Option<String>, String, u64)>> {
        let mut counts = vec![];
        for name in self.db.list_collection_names().await? {
            if let Ok(count) = self.coll(&name).estimated_document_count().await {
                counts.push((None, name, count));
            }
        }
        Ok(counts)
    }

    /// Sizes on disk, from listDatabases.
    pub async fn database_stats(&self) -> Result<Vec<DatabaseStats>> {
        Ok(self
            .client
            .list_databases()
            .await?
            .into_iter()
            .map(|database| DatabaseStats {
                name: database.name,
                size_bytes: Some(database.size_on_disk),
                objects: None,
            })
            .collect())
    }

    pub fn use_database(&mut self, name: &str) -> Result<()> {
        self.db = self.client.database(name);
        Ok(())
    }

    pub async fn list_objects(&self) -> Result<Vec<DbObject>> {
        let mut names = self.db.list_collection_names().await?;
        names.sort();
        Ok(names
            .into_iter()
            .map(|name| DbObject {
                schema: None,
                name,
                kind: ObjectKind::Collection,
            })
            .collect())
    }

    pub async fn structure(&self, obj: &DbObject) -> Result<Structure> {
        let docs: Vec<Document> = self
            .coll(&obj.name)
            .find(doc! {})
            .limit(100)
            .await?
            .try_collect()
            .await?;
        let mut columns: Vec<ColumnInfo> = Vec::new();
        for d in &docs {
            for (k, v) in d {
                if !columns.iter().any(|c| &c.name == k) {
                    let c = ColumnInfo {
                        name: k.clone(),
                        data_type: bson_type(v).into(),
                        nullable: k != "_id",
                        default: None,
                        is_pk: k == "_id",
                        enum_values: vec![],
                    };
                    if k == "_id" {
                        columns.insert(0, c)
                    } else {
                        columns.push(c)
                    }
                }
            }
        }
        let idx: Vec<_> = self
            .coll(&obj.name)
            .list_indexes()
            .await?
            .try_collect()
            .await?;
        let indexes = QueryResult {
            columns: vec!["name".into(), "keys".into(), "unique".into()],
            rows: idx
                .into_iter()
                .map(|m| {
                    let o = m.options.unwrap_or_default();
                    vec![
                        o.name,
                        Some(Bson::Document(m.keys).into_relaxed_extjson().to_string()),
                        Some(o.unique.unwrap_or(false).to_string()),
                    ]
                })
                .collect(),
            ..Default::default()
        };
        Ok(Structure {
            columns,
            indexes,
            ..Default::default()
        })
    }

    pub async fn fetch_table(&self, q: &TableQuery) -> Result<(QueryResult, Option<u64>)> {
        let mut clauses: Vec<Document> = Vec::new();
        for f in q.filters.iter().filter(|f| !f.column.is_empty()) {
            let parsed = parse_json5(&f.value).unwrap_or_else(|_| Bson::String(f.value.clone()));
            let cond: Bson = match f.op {
                // Match the text both as a string and as its parsed type.
                FilterOp::Eq => doc! {"$in": [Bson::String(f.value.clone()), parsed]}.into(),
                FilterOp::Ne => doc! {"$nin": [Bson::String(f.value.clone()), parsed]}.into(),
                FilterOp::Lt => doc! {"$lt": parsed}.into(),
                FilterOp::Gt => doc! {"$gt": parsed}.into(),
                FilterOp::Le => doc! {"$lte": parsed}.into(),
                FilterOp::Ge => doc! {"$gte": parsed}.into(),
                FilterOp::Contains => {
                    doc! {"$regex": regex_escape(&f.value), "$options": "i"}.into()
                }
                FilterOp::NotContains => {
                    doc! {"$not": {"$regex": regex_escape(&f.value), "$options": "i"}}.into()
                }
                FilterOp::StartsWith => {
                    doc! {"$regex": format!("^{}", regex_escape(&f.value)), "$options": "i"}.into()
                }
                FilterOp::EndsWith => {
                    doc! {"$regex": format!("{}$", regex_escape(&f.value)), "$options": "i"}.into()
                }
                FilterOp::Like => doc! {"$regex": like_to_regex(&f.value)}.into(),
                FilterOp::Regex => doc! {"$regex": f.value.clone()}.into(),
                FilterOp::In | FilterOp::NotIn => {
                    let items = FilterOp::list(&f.value);
                    if items.is_empty() {
                        continue;
                    }
                    // Each value as text and as its parsed type, like `=`.
                    let values: Vec<Bson> = items
                        .iter()
                        .flat_map(|item| {
                            let typed =
                                parse_json5(item).unwrap_or_else(|_| Bson::String(item.clone()));
                            [Bson::String(item.clone()), typed]
                        })
                        .collect();
                    let op = if f.op == FilterOp::In { "$in" } else { "$nin" };
                    doc! {op: values}.into()
                }
                FilterOp::Between => {
                    let upper =
                        parse_json5(&f.value2).unwrap_or_else(|_| Bson::String(f.value2.clone()));
                    doc! {"$gte": parsed, "$lte": upper}.into()
                }
                FilterOp::IsNull => Bson::Null,
                FilterOp::IsNotNull => doc! {"$ne": Bson::Null}.into(),
            };
            clauses.push(doc! {f.column.clone(): cond});
        }
        // `$and`, so two filters on one field (age > 18, age < 30) both apply;
        // `$or` for "any".
        let filter = match clauses.len() {
            0 => Document::new(),
            1 => clauses.pop().unwrap(),
            _ if q.match_any => doc! {"$or": clauses},
            _ => doc! {"$and": clauses},
        };
        // The find bar's document applies on top of the filters.
        let filter = match q.find.trim() {
            "" => filter,
            find => {
                let Bson::Document(find) = parse_json5(find)
                    .map_err(|error| anyhow!("the find filter isn't valid JSON: {error}"))?
                else {
                    bail!("the find filter must be a document, like {{ status: \"paid\" }}");
                };
                if filter.is_empty() {
                    find
                } else {
                    doc! {"$and": [find, filter]}
                }
            }
        };
        let coll = self.coll(&q.object.name);
        let mut find = coll
            .find(filter.clone())
            .skip(q.offset as u64)
            .limit(q.limit as i64);
        if let Some((c, asc)) = &q.order {
            find = find.sort(doc! {c.clone(): if *asc { 1 } else { -1 }});
        }
        let docs: Vec<Document> = find.await?.try_collect().await?;
        let total = if filter.is_empty() {
            coll.estimated_document_count().await.ok()
        } else {
            coll.count_documents(filter).await.ok()
        };
        Ok((docs_to_result(docs), total))
    }

    pub async fn execute(&self, text: &str) -> Result<Vec<QueryResult>> {
        let start = Instant::now();
        let text = text.trim().trim_end_matches(';').trim();
        let mut res = self.run(text).await?;
        res.statement = text.to_string();
        res.elapsed_ms = start.elapsed().as_millis();
        Ok(vec![res])
    }

    async fn run(&self, text: &str) -> Result<QueryResult> {
        if text.starts_with('{') {
            let out = self.db.run_command(parse_doc(text)?).await?;
            return Ok(docs_to_result(vec![out]));
        }
        let rest = text.strip_prefix("db.").ok_or_else(|| {
            anyhow!("expected db.<collection>.<method>(…) or a {{command}} document")
        })?;
        // db.getCollection("name").x(...) or db.name.x(...)
        let (coll_name, rest) = if let Some(r) = rest.strip_prefix("getCollection(") {
            let close = r.find(')').context("missing )")?;
            (
                r[..close]
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_string(),
                &r[close + 1..],
            )
        } else {
            let dot = rest
                .find('.')
                .context("expected db.<collection>.<method>(…)")?;
            (rest[..dot].to_string(), &rest[dot..])
        };
        let rest = rest.strip_prefix('.').context("expected .<method>(…)")?;
        let open = rest.find('(').context("expected (")?;
        let method = rest[..open].trim();
        let (inner, mut chain) = take_call(rest, open)?;
        let args = parse_args(inner)?;
        let coll = self.coll(&coll_name);

        let count_row = |label: &str, n: u64| QueryResult {
            columns: vec![label.into()],
            rows: vec![vec![Some(n.to_string())]],
            affected: Some(n),
            ..Default::default()
        };

        Ok(match method {
            "find" | "findOne" => {
                let mut find = coll.find(arg_doc(&args, 0)?).projection(arg_doc(&args, 1)?);
                let mut limit: i64 = if method == "findOne" { 1 } else { 1000 };
                // .sort({...}).limit(n).skip(n)
                while let Some(r) = chain.trim_start().strip_prefix('.') {
                    let open = r.find('(').context("expected (")?;
                    let m = r[..open].trim().to_string();
                    let (inner, next) = take_call(r, open)?;
                    match m.as_str() {
                        "sort" => find = find.sort(parse_doc(inner)?),
                        "limit" => limit = inner.trim().parse()?,
                        "skip" => find = find.skip(inner.trim().parse()?),
                        "pretty" | "toArray" => {}
                        other => bail!("unsupported cursor method .{other}()"),
                    }
                    chain = next;
                }
                let docs: Vec<Document> = find.limit(limit).await?.try_collect().await?;
                docs_to_result(docs)
            }
            "aggregate" => {
                let pipeline = match args.into_iter().next() {
                    Some(Bson::Array(stages)) => stages
                        .into_iter()
                        .map(|s| match s {
                            Bson::Document(d) => Ok(d),
                            _ => Err(anyhow!("each pipeline stage must be a document")),
                        })
                        .collect::<Result<Vec<_>>>()?,
                    _ => bail!("aggregate expects an array of stages"),
                };
                docs_to_result(coll.aggregate(pipeline).await?.try_collect().await?)
            }
            "countDocuments" | "count" => {
                count_row("count", coll.count_documents(arg_doc(&args, 0)?).await?)
            }
            "insertOne" => {
                let r = coll.insert_one(arg_doc(&args, 0)?).await?;
                docs_to_result(vec![doc! {"insertedId": r.inserted_id}])
            }
            "insertMany" => {
                let docs = match args.into_iter().next() {
                    Some(Bson::Array(a)) => a
                        .into_iter()
                        .filter_map(|b| {
                            if let Bson::Document(d) = b {
                                Some(d)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>(),
                    _ => bail!("insertMany expects an array of documents"),
                };
                count_row(
                    "inserted",
                    coll.insert_many(docs).await?.inserted_ids.len() as u64,
                )
            }
            "updateOne" => count_row(
                "modified",
                coll.update_one(arg_doc(&args, 0)?, arg_doc(&args, 1)?)
                    .await?
                    .modified_count,
            ),
            "updateMany" => count_row(
                "modified",
                coll.update_many(arg_doc(&args, 0)?, arg_doc(&args, 1)?)
                    .await?
                    .modified_count,
            ),
            "deleteOne" => count_row(
                "deleted",
                coll.delete_one(arg_doc(&args, 0)?).await?.deleted_count,
            ),
            "deleteMany" => count_row(
                "deleted",
                coll.delete_many(arg_doc(&args, 0)?).await?.deleted_count,
            ),
            "drop" => {
                coll.drop().await?;
                count_row("dropped", 1)
            }
            other => bail!("unsupported method {other}()"),
        })
    }

    pub async fn commit(&self, obj: &DbObject, changes: &[Change]) -> Result<()> {
        let types = self.structure(obj).await?.columns;
        let ty = |c: &str| {
            types
                .iter()
                .find(|t| t.name == c)
                .map(|t| t.data_type.as_str())
        };
        let coll = self.coll(&obj.name);
        let id_filter = |key: &[(String, Option<String>)]| -> Result<Document> {
            let (_, v) = key
                .iter()
                .find(|(c, _)| c == "_id")
                .context("documents without _id can't be edited")?;
            Ok(doc! {"_id": to_bson(v, ty("_id"))})
        };
        // MongoDB transactions need a replica set, so changes apply one at a
        // time; a failure says how many were already saved.
        let n = changes.len();
        for (i, ch) in changes.iter().enumerate() {
            let applied: Result<()> = async {
                match ch {
                    Change::Update { key, set } => {
                        let mut s = Document::new();
                        let mut unset = Document::new();
                        for (c, v) in set {
                            match v {
                                None => {
                                    unset.insert(c.clone(), "");
                                }
                                Some(_) => {
                                    s.insert(c.clone(), to_bson(v, ty(c)));
                                }
                            }
                        }
                        let mut update = Document::new();
                        if !s.is_empty() {
                            update.insert("$set", s);
                        }
                        if !unset.is_empty() {
                            update.insert("$unset", unset);
                        }
                        let r = coll.update_one(id_filter(key)?, update).await?;
                        if r.matched_count != 1 {
                            bail!("document not found; it may have been changed elsewhere");
                        }
                    }
                    Change::Insert { values } => {
                        let mut d = Document::new();
                        for (c, v) in values.iter().filter(|(_, v)| v.is_some()) {
                            d.insert(c.clone(), to_bson(v, ty(c)));
                        }
                        coll.insert_one(d).await?;
                    }
                    Change::Delete { key } => {
                        let r = coll.delete_one(id_filter(key)?).await?;
                        if r.deleted_count != 1 {
                            bail!("document not found; it may have been deleted elsewhere");
                        }
                    }
                }
                Ok(())
            }
            .await;
            if let Err(e) = applied {
                return Err(e.context(format!(
                    "change {} of {n} failed; the {i} before it were saved",
                    i + 1
                )));
            }
        }
        Ok(())
    }

    pub async fn drop_object(&self, obj: &DbObject) -> Result<()> {
        Ok(self.coll(&obj.name).drop().await?)
    }

    pub async fn truncate(&self, obj: &DbObject) -> Result<()> {
        self.coll(&obj.name).delete_many(doc! {}).await?;
        Ok(())
    }
}

pub fn preview(obj: &DbObject, changes: &[Change]) -> String {
    let fmt = |kv: &[(String, Option<String>)]| {
        kv.iter()
            .map(|(c, v)| format!("{c}: {}", v.as_deref().unwrap_or("null")))
            .collect::<Vec<_>>()
            .join(", ")
    };
    changes
        .iter()
        .map(|ch| match ch {
            Change::Update { key, set } => format!(
                "db.{}.updateOne({{{}}}, {{$set: {{{}}}}})",
                obj.name,
                fmt(key),
                fmt(set)
            ),
            Change::Insert { values } => format!("db.{}.insertOne({{{}}})", obj.name, fmt(values)),
            Change::Delete { key } => format!("db.{}.deleteOne({{{}}})", obj.name, fmt(key)),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A SQL LIKE pattern as an anchored regex: `%` any run, `_` one character.
fn like_to_regex(pattern: &str) -> String {
    let mut out = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '%' => out.push_str(".*"),
            '_' => out.push('.'),
            other => out.push_str(&regex_escape(&other.to_string())),
        }
    }
    out.push('$');
    out
}

fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if "\\.^$|?*+()[]{}".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mongosh_syntax() {
        let args =
            parse_args(r#"{_id: ObjectId("64b7f0c2a1b2c3d4e5f60718"), age: {$gt: 30}}, {name: 1}"#)
                .unwrap();
        assert_eq!(args.len(), 2);
        let Bson::Document(d) = &args[0] else {
            panic!()
        };
        assert!(matches!(d.get("_id"), Some(Bson::ObjectId(_))));
        assert_eq!(
            render(d.get("_id").unwrap()).unwrap(),
            r#"ObjectId("64b7f0c2a1b2c3d4e5f60718")"#
        );
        // Rendered ObjectId parses back to an ObjectId.
        assert!(matches!(
            to_bson(
                &Some(r#"ObjectId("64b7f0c2a1b2c3d4e5f60718")"#.into()),
                Some("objectId")
            ),
            Bson::ObjectId(_)
        ));
    }

    #[test]
    fn take_call_skips_nested() {
        let (inner, rest) = take_call(r#"find({a: "(x)"}).limit(2)"#, 4).unwrap();
        assert_eq!(inner, r#"{a: "(x)"}"#);
        assert_eq!(rest, ".limit(2)");
    }
}
