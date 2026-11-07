//! EXPLAIN as a tree: Postgres `EXPLAIN (FORMAT JSON)`, MySQL
//! `EXPLAIN FORMAT=TREE`, SQLite `EXPLAIN QUERY PLAN`, each parsed into
//! `PlanNode`s with costs / row estimates (and actual numbers with ANALYZE).

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use super::{Driver, QueryResult};
use serde_json::{from_str, to_string_pretty};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanNode {
    /// "Seq Scan on users", "Nested loop inner join"…
    pub label: String,
    /// Conditions, index used… one per line.
    pub details: Vec<String>,
    /// Estimated total cost (planner units).
    pub cost: Option<f64>,
    pub rows: Option<f64>,
    /// With ANALYZE: total milliseconds and rows actually produced.
    pub actual_ms: Option<f64>,
    pub actual_rows: Option<f64>,
    pub children: Vec<PlanNode>,
}

impl PlanNode {
    /// Depth-first, with depth.
    pub fn flatten(&self) -> Vec<(usize, &PlanNode)> {
        fn walk<'a>(n: &'a PlanNode, d: usize, out: &mut Vec<(usize, &'a PlanNode)>) {
            out.push((d, n));
            for c in &n.children {
                walk(c, d + 1, out);
            }
        }
        let mut out = vec![];
        walk(self, 0, &mut out);
        out
    }

    /// The node's own share: its time (or cost) minus its children's.
    pub fn self_weight(&self) -> f64 {
        let w = |n: &PlanNode| n.actual_ms.or(n.cost).unwrap_or(0.0);
        (w(self) - self.children.iter().map(w).sum::<f64>()).max(0.0)
    }
}

/// One plain-language tip for a plan's most expensive step (design.md,
/// screen 14), with SQL to copy when there's an obvious fix.
#[derive(Clone, Debug, PartialEq)]
pub struct Insight {
    pub text: String,
    pub sql: Option<String>,
}

/// Plans cheaper than this get no tip (cost units, or ms with ANALYZE).
const CHEAP_COST: f64 = 1000.0;
const CHEAP_MS: f64 = 20.0;

/// The single tip for `root`, or `None` for a cheap plan or nothing useful
/// to say. Never more than one.
pub fn insight(root: &PlanNode) -> Option<Insight> {
    let analyzed = root.actual_ms.is_some();
    let total = root.actual_ms.or(root.cost).unwrap_or(0.0);
    if total < if analyzed { CHEAP_MS } else { CHEAP_COST } {
        return None;
    }
    let nodes = root.flatten();
    let (_, hot) = nodes
        .iter()
        .max_by(|a, b| a.1.self_weight().total_cmp(&b.1.self_weight()))?;
    let share = hot.self_weight() / total;
    if share < 0.3 {
        return None;
    }
    let pct = (share * 100.0).round();
    // Postgres "Seq Scan on orders o"; MySQL "Table scan on orders".
    let table = hot
        .label
        .strip_prefix("Seq Scan on ")
        .or_else(|| hot.label.strip_prefix("Table scan on "))
        .and_then(|rest| rest.split_whitespace().next());
    if let Some(table) = table {
        let filter = hot.details.iter().find_map(|d| d.strip_prefix("Filter: "));
        let removed = hot.details.iter().find_map(|d| {
            d.strip_prefix("Rows removed by filter: ")?
                .parse::<f64>()
                .ok()
        });
        let kept = hot.actual_rows.unwrap_or(0.0);
        let column = filter.and_then(filter_column);
        let discards = match removed {
            Some(r) if r + kept > 0.0 => {
                format!(", throwing away {:.0}% of them", r / (r + kept) * 100.0)
            }
            _ => String::new(),
        };
        let text = match &column {
            Some(c) => format!(
                "Reading every row of {table}{discards} takes {pct}% of this plan. An index on {table}({c}) would let it read only the matching rows."
            ),
            None => format!("Reading every row of {table}{discards} takes {pct}% of this plan."),
        };
        return Some(Insight {
            text,
            sql: column.map(|c| format!("CREATE INDEX ON {table} ({c});")),
        });
    }
    if hot.label.starts_with("Sort") {
        return Some(Insight {
            text: format!(
                "Sorting takes {pct}% of this plan. An index in the ORDER BY order would return rows already sorted."
            ),
            sql: None,
        });
    }
    None
}

/// The column a simple filter tests: `(status = 'paid'::text)` → status.
fn filter_column(cond: &str) -> Option<String> {
    let inner = cond.trim().trim_start_matches('(').trim_start();
    let word: String = inner
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();
    let col = word.rsplit('.').next()?.to_string();
    // Skip what Postgres wraps a column in: `(status)::text`.
    let mut rest = inner[word.len()..].trim_start();
    loop {
        if let Some(r) = rest.strip_prefix(')') {
            rest = r.trim_start();
        } else if let Some(r) = rest.strip_prefix("::") {
            let ty = r
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ' '))
                .unwrap_or(r.len());
            // Types can have spaces ("character varying"); the operator ends it.
            rest = r[ty..].trim_start();
        } else {
            break;
        }
    }
    let compares = ["=", "<", ">", "!=", "<>", "IS ", "LIKE", "~~", "IN "]
        .iter()
        .any(|op| rest.to_uppercase().starts_with(op));
    (!col.is_empty() && !col.chars().all(|c| c.is_ascii_digit()) && compares).then_some(col)
}

/// The server's answer as text, for the Raw view.
pub fn raw_text(driver: Driver, r: &QueryResult) -> String {
    let cells: Vec<String> = r
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|c| c.clone().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .collect();
    let text = cells.join("\n");
    match driver {
        Driver::Postgres => from_str::<Value>(&text)
            .ok()
            .and_then(|v| to_string_pretty(&v).ok())
            .unwrap_or(text),
        _ => text,
    }
}

/// The statement that asks the server for a plan.
pub fn explain_sql(driver: Driver, stmt: &str, analyze: bool) -> Result<String> {
    let stmt = stmt.trim().trim_end_matches(';');
    Ok(match (driver, analyze) {
        (Driver::Postgres, false) => format!("EXPLAIN (FORMAT JSON) {stmt}"),
        (Driver::Postgres, true) => format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {stmt}"),
        (Driver::MySql, false) => format!("EXPLAIN FORMAT=TREE {stmt}"),
        (Driver::MySql, true) => format!("EXPLAIN ANALYZE {stmt}"),
        (Driver::Sqlite, _) => format!("EXPLAIN QUERY PLAN {stmt}"),
        _ => bail!("EXPLAIN is available for Postgres, MySQL and SQLite"),
    })
}

/// What to run: with `analyze` (which executes the statement) wrapped in a
/// transaction that's rolled back, so a write is measured but not kept.
pub fn explain_script(driver: Driver, stmt: &str, analyze: bool) -> Result<String> {
    let sql = explain_sql(driver, stmt, analyze)?;
    // `;` on its own line: a trailing `-- comment` must not swallow it.
    Ok(match (analyze, driver) {
        (true, Driver::Postgres) => format!("BEGIN;\n{sql}\n;\nROLLBACK;"),
        (true, Driver::MySql) => format!("START TRANSACTION;\n{sql}\n;\nROLLBACK;"),
        _ => sql,
    })
}

/// Turns the server's answer to `explain_sql` into a tree.
pub fn parse(driver: Driver, r: &QueryResult) -> Result<PlanNode> {
    let cell = |row: usize, col: usize| {
        r.rows
            .get(row)
            .and_then(|x| x.get(col))
            .cloned()
            .flatten()
            .unwrap_or_default()
    };
    match driver {
        Driver::Postgres => {
            let text: String = (0..r.rows.len()).map(|i| cell(i, 0)).collect();
            let v: Value = from_str(&text).context("reading the JSON plan")?;
            let plan = v
                .get(0)
                .and_then(|p| p.get("Plan"))
                .context("no plan in the server's answer")?;
            let mut root = pg_node(plan);
            // Planning / execution totals.
            if let Some(t) = v[0].get("Execution Time").and_then(Value::as_f64) {
                root.details.insert(0, format!("Execution time: {t:.3} ms"));
            }
            if let Some(t) = v[0].get("Planning Time").and_then(Value::as_f64) {
                root.details.insert(0, format!("Planning time: {t:.3} ms"));
            }
            Ok(root)
        }
        Driver::MySql => mysql_tree(&cell(0, 0)),
        Driver::Sqlite => {
            // Rows: id, parent, notused, detail.
            let rows: Vec<(i64, i64, String)> = (0..r.rows.len())
                .map(|i| {
                    (
                        cell(i, 0).parse().unwrap_or(0),
                        cell(i, 1).parse().unwrap_or(0),
                        cell(i, 3),
                    )
                })
                .collect();
            fn kids(parent: i64, rows: &[(i64, i64, String)]) -> Vec<PlanNode> {
                rows.iter()
                    .filter(|r| r.1 == parent)
                    .map(|r| PlanNode {
                        label: r.2.clone(),
                        children: kids(r.0, rows),
                        ..Default::default()
                    })
                    .collect()
            }
            Ok(PlanNode {
                label: "Query plan".into(),
                children: kids(0, &rows),
                ..Default::default()
            })
        }
        _ => bail!("EXPLAIN isn't available for this database"),
    }
}

fn pg_node(p: &Value) -> PlanNode {
    let s = |k: &str| p.get(k).and_then(Value::as_str).map(String::from);
    let f = |k: &str| p.get(k).and_then(Value::as_f64);
    let mut label = s("Node Type").unwrap_or_else(|| "?".into());
    if let Some(j) = s("Join Type").filter(|_| label.contains("Join") || label == "Nested Loop") {
        label = format!("{label} ({j})");
    }
    if let Some(rel) = s("Relation Name") {
        label.push_str(&format!(" on {rel}"));
        if let Some(a) = s("Alias").filter(|a| *a != rel) {
            label.push_str(&format!(" {a}"));
        }
    }
    if let Some(i) = s("Index Name") {
        label.push_str(&format!(" using {i}"));
    }
    let mut details = vec![];
    for k in [
        "Index Cond",
        "Filter",
        "Hash Cond",
        "Merge Cond",
        "Join Filter",
        "Recheck Cond",
        "Sort Key",
        "Group Key",
    ] {
        match p.get(k) {
            Some(Value::String(v)) => details.push(format!("{k}: {v}")),
            Some(Value::Array(a)) => details.push(format!(
                "{k}: {}",
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            _ => {}
        }
    }
    if let Some(n) = f("Rows Removed by Filter").filter(|n| *n > 0.0) {
        details.push(format!("Rows removed by filter: {n}"));
    }
    let loops = f("Actual Loops").unwrap_or(1.0);
    PlanNode {
        label,
        details,
        cost: f("Total Cost"),
        rows: f("Plan Rows"),
        // Per-loop numbers × loops = the node's total.
        actual_ms: f("Actual Total Time").map(|t| t * loops),
        actual_rows: f("Actual Rows").map(|r| r * loops),
        children: p
            .get("Plans")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(pg_node).collect())
            .unwrap_or_default(),
    }
}

/// `(cost=1.25 rows=3)` / `(actual time=0.1..0.2 rows=3 loops=1)`.
fn numbers(s: &str, key: &str) -> Option<String> {
    let i = s.find(key)? + key.len();
    let rest = &s[i..];
    let end = rest.find([' ', ')']).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn mysql_tree(text: &str) -> Result<PlanNode> {
    if text.contains("not executable by iterator executor") {
        bail!(
            "MySQL can only EXPLAIN ANALYZE queries that read (SELECT / TABLE); use Explain instead"
        );
    }
    let mut root = PlanNode {
        label: "Query plan".into(),
        ..Default::default()
    };
    // Stack of (indent, path of child indexes from root).
    let mut stack: Vec<(usize, Vec<usize>)> = vec![];
    for line in text.lines() {
        let Some(pos) = line.find("-> ") else {
            continue;
        };
        let body = &line[pos + 3..];
        let (label, rest) = match body.find("  (") {
            Some(i) => (&body[..i], &body[i..]),
            None => (body, ""),
        };
        let mut node = PlanNode {
            label: label.trim().to_string(),
            cost: numbers(rest, "cost=").and_then(|v| v.parse().ok()),
            rows: numbers(rest, "rows=").and_then(|v| v.parse().ok()),
            ..Default::default()
        };
        if let Some(a) = rest.find("(actual ") {
            let act = &rest[a..];
            let loops: f64 = numbers(act, "loops=")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0);
            node.actual_ms = numbers(act, "time=")
                .and_then(|t| t.split("..").nth(1).and_then(|e| e.parse::<f64>().ok()))
                .map(|t| t * loops);
            node.actual_rows = numbers(act, "rows=")
                .and_then(|v| v.parse::<f64>().ok())
                .map(|r| r * loops);
        }
        while stack.last().is_some_and(|(ind, _)| *ind >= pos) {
            stack.pop();
        }
        let parent_path = stack.last().map(|(_, p)| p.clone()).unwrap_or_default();
        let mut parent = &mut root;
        for i in &parent_path {
            parent = &mut parent.children[*i];
        }
        parent.children.push(node);
        let mut path = parent_path;
        path.push(parent.children.len() - 1);
        stack.push((pos, path));
    }
    if root.children.is_empty() {
        bail!("couldn't read the plan; the server may not support EXPLAIN FORMAT=TREE");
    }
    // A single top node is the plan itself.
    Ok(if root.children.len() == 1 {
        root.children.remove(0)
    } else {
        root
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(rows: &[&[&str]]) -> QueryResult {
        QueryResult {
            rows: rows
                .iter()
                .map(|r| r.iter().map(|c| Some(c.to_string())).collect())
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn postgres_json_plan() {
        let json = r#"[{"Plan": {"Node Type": "Hash Join", "Join Type": "Inner", "Total Cost": 30.5,
            "Plan Rows": 10, "Actual Total Time": 2.0, "Actual Rows": 10, "Actual Loops": 1,
            "Hash Cond": "(o.user_id = u.id)",
            "Plans": [
              {"Node Type": "Seq Scan", "Relation Name": "orders", "Alias": "o", "Total Cost": 20.0,
               "Plan Rows": 100, "Actual Total Time": 1.5, "Actual Rows": 100, "Actual Loops": 1,
               "Filter": "(paid)", "Rows Removed by Filter": 5},
              {"Node Type": "Index Scan", "Relation Name": "users", "Alias": "users", "Index Name": "users_pkey",
               "Total Cost": 8.0, "Plan Rows": 1, "Actual Total Time": 0.01, "Actual Rows": 1, "Actual Loops": 10}
            ]}, "Planning Time": 0.2, "Execution Time": 2.1}]"#;
        let root = parse(Driver::Postgres, &result(&[&[json]])).unwrap();
        assert_eq!(root.label, "Hash Join (Inner)");
        assert_eq!(root.details[0], "Planning time: 0.200 ms");
        assert!(
            root.details
                .contains(&"Hash Cond: (o.user_id = u.id)".to_string())
        );
        assert_eq!(root.children[0].label, "Seq Scan on orders o");
        assert!(
            root.children[0]
                .details
                .contains(&"Rows removed by filter: 5".to_string())
        );
        assert_eq!(
            root.children[1].label,
            "Index Scan on users using users_pkey"
        );
        // 0.01 ms × 10 loops.
        assert_eq!(root.children[1].actual_ms, Some(0.1));
        assert_eq!(root.flatten().len(), 3);
        assert!((root.self_weight() - 0.4).abs() < 1e-9);
    }

    #[test]
    fn mysql_tree_plan() {
        let text = "-> Nested loop inner join  (cost=2.2 rows=3) (actual time=0.05..0.08 rows=3 loops=1)\n    -> Filter: (o.paid = 1)  (cost=1.1 rows=3)\n        -> Table scan on o  (cost=1.1 rows=6)\n    -> Single-row index lookup on u using PRIMARY (id=o.user_id)  (cost=0.3 rows=1)\n";
        let root = mysql_tree(text).unwrap();
        assert_eq!(root.label, "Nested loop inner join");
        assert_eq!(root.cost, Some(2.2));
        assert_eq!(root.actual_ms, Some(0.08));
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].children[0].label, "Table scan on o");
        assert_eq!(root.children[0].children[0].rows, Some(6.0));
        assert!(
            root.children[1]
                .label
                .starts_with("Single-row index lookup")
        );
    }

    #[test]
    fn sqlite_query_plan() {
        let r = result(&[
            &["2", "0", "0", "SCAN o"],
            &[
                "5",
                "0",
                "0",
                "SEARCH u USING INTEGER PRIMARY KEY (rowid=?)",
            ],
            &["7", "5", "0", "nested"],
        ]);
        let root = parse(Driver::Sqlite, &r).unwrap();
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[1].children[0].label, "nested");
    }

    fn scan(rows: f64, removed: f64, ms: f64) -> PlanNode {
        PlanNode {
            label: "Seq Scan on orders o".into(),
            details: vec![
                "Filter: ((status)::text = 'pending'::text)".into(),
                format!("Rows removed by filter: {removed}"),
            ],
            cost: Some(5000.0),
            actual_ms: Some(ms),
            actual_rows: Some(rows),
            ..Default::default()
        }
    }

    #[test]
    fn one_insight_for_the_hottest_step() {
        let root = PlanNode {
            label: "Hash Join".into(),
            cost: Some(6000.0),
            actual_ms: Some(100.0),
            children: vec![scan(50.0, 950.0, 90.0)],
            ..Default::default()
        };
        let i = insight(&root).expect("a tip");
        assert!(i.text.contains("throwing away 95%"), "{}", i.text);
        assert!(i.text.contains("90%"), "{}", i.text);
        assert_eq!(i.sql.as_deref(), Some("CREATE INDEX ON orders (status);"));

        // Cheap plans get nothing.
        let cheap = PlanNode {
            actual_ms: Some(1.0),
            children: vec![scan(5.0, 5.0, 0.9)],
            ..Default::default()
        };
        assert_eq!(insight(&cheap), None);
    }

    #[test]
    fn filter_columns() {
        assert_eq!(
            filter_column("(status = 'paid'::text)").as_deref(),
            Some("status")
        );
        assert_eq!(filter_column("(o.total > 100)").as_deref(), Some("total"));
        assert_eq!(filter_column("(lower(name) = 'x')"), None);
        assert_eq!(filter_column("(paid)"), None);
        assert_eq!(
            filter_column("((status)::text = 'pending'::text)").as_deref(),
            Some("status")
        );
        assert_eq!(
            filter_column("((email)::character varying ~~ '%x'::text)").as_deref(),
            Some("email")
        );
    }
}
