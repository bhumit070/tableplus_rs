//! One Redis key in the key editor (design.md screen 19): its type, TTL,
//! size and first elements, and the staged edits Save turns into HSET /
//! LSET / SADD / ZADD / … in one MULTI.

use anyhow::{Result, bail};
use redis::{Value, cmd, pipe};
use uuid::Uuid;

use super::redis_kv::RedisConn;

/// Elements read per key; the editor says when there are more.
pub const KEY_ITEMS_CAP: usize = 500;

/// Strings past this many bytes are read only up to it.
const STRING_CAP: usize = 1 << 20;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyDetail {
    pub key: String,
    /// `string`, `hash`, `list`, `set`, `zset`, `stream`; `none` if gone.
    pub kind: String,
    /// Seconds left; `None` = no expiry.
    pub ttl: Option<i64>,
    /// Bytes, from MEMORY USAGE (not every server allows it).
    pub size: Option<u64>,
    /// Elements (fields, members, entries), or the string's length.
    pub length: u64,
    /// hash (field, value) · list (index, value) · set (member, "") ·
    /// zset (member, score) · stream (id, "field=value, …") · string ("", text).
    pub items: Vec<(String, String)>,
    /// Fewer items shown than the key holds.
    pub truncated: bool,
}

/// One staged change to a key's contents.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyEdit {
    SetString(String),
    HashSet { field: String, value: String },
    HashDelete(String),
    ListSet { index: i64, value: String },
    ListPush(String),
    ListRemove(i64),
    SetAdd(String),
    SetRemove(String),
    ZsetAdd { member: String, score: f64 },
    ZsetRemove(String),
}

/// Every string in a reply, in order, however RESP2 / RESP3 nests them.
fn flatten(value: Value, out: &mut Vec<String>) {
    match value {
        Value::Array(items) | Value::Set(items) => {
            for item in items {
                flatten(item, out);
            }
        }
        Value::Map(pairs) => {
            for (key, value) in pairs {
                flatten(key, out);
                flatten(value, out);
            }
        }
        Value::BulkString(bytes) => out.push(String::from_utf8_lossy(&bytes).into_owned()),
        Value::SimpleString(text) => out.push(text),
        Value::VerbatimString { text, .. } => out.push(text),
        Value::Int(number) => out.push(number.to_string()),
        Value::Double(number) => out.push(number.to_string()),
        Value::Boolean(flag) => out.push(flag.to_string()),
        Value::BigNumber(number) => out.push(number.to_string()),
        _ => {}
    }
}

fn strings(value: Value) -> Vec<String> {
    let mut out = vec![];
    flatten(value, &mut out);
    out
}

fn pairs(flat: Vec<String>) -> Vec<(String, String)> {
    let mut iter = flat.into_iter();
    let mut out = vec![];
    while let (Some(first), Some(second)) = (iter.next(), iter.next()) {
        out.push((first, second));
    }
    out
}

/// The commands `edits` become, as Save would send them (for Preview).
pub fn edit_commands(key: &str, edits: &[KeyEdit]) -> Vec<Vec<String>> {
    // List removals: mark each by index, then remove the marks, so earlier
    // removals don't shift the indexes of later ones.
    let marker = format!("__tabula_removed_{}__", Uuid::new_v4().simple());
    let key = key.to_string();
    let mut commands: Vec<Vec<String>> = vec![];
    let mut appends = vec![];
    let mut removes_from_list = false;
    for edit in edits {
        let command: Vec<String> = match edit {
            KeyEdit::SetString(text) => {
                vec!["SET".into(), key.clone(), text.clone(), "KEEPTTL".into()]
            }
            KeyEdit::HashSet { field, value } => {
                vec!["HSET".into(), key.clone(), field.clone(), value.clone()]
            }
            KeyEdit::HashDelete(field) => vec!["HDEL".into(), key.clone(), field.clone()],
            KeyEdit::ListSet { index, value } => {
                vec!["LSET".into(), key.clone(), index.to_string(), value.clone()]
            }
            KeyEdit::ListPush(value) => {
                appends.push(vec!["RPUSH".into(), key.clone(), value.clone()]);
                continue;
            }
            KeyEdit::ListRemove(index) => {
                removes_from_list = true;
                vec![
                    "LSET".into(),
                    key.clone(),
                    index.to_string(),
                    marker.clone(),
                ]
            }
            KeyEdit::SetAdd(member) => vec!["SADD".into(), key.clone(), member.clone()],
            KeyEdit::SetRemove(member) => vec!["SREM".into(), key.clone(), member.clone()],
            KeyEdit::ZsetAdd { member, score } => {
                vec![
                    "ZADD".into(),
                    key.clone(),
                    score.to_string(),
                    member.clone(),
                ]
            }
            KeyEdit::ZsetRemove(member) => vec!["ZREM".into(), key.clone(), member.clone()],
        };
        commands.push(command);
    }
    if removes_from_list {
        commands.push(vec!["LREM".into(), key.clone(), "0".into(), marker]);
    }
    commands.extend(appends);
    commands
}

/// `HSET "user:1" "name" "Ana"`, one command per line, for Preview.
pub fn preview(key: &str, edits: &[KeyEdit]) -> String {
    edit_commands(key, edits)
        .iter()
        .map(|command| {
            let (name, arguments) = command.split_first().expect("commands have a name");
            let quoted: Vec<String> = arguments
                .iter()
                .map(|argument| format!("{argument:?}"))
                .collect();
            format!("{name} {}", quoted.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl RedisConn {
    /// Type, TTL, size and up to `KEY_ITEMS_CAP` elements of `key`.
    pub async fn key_detail(&self, key: &str) -> Result<KeyDetail> {
        let mut con = self.con.clone();
        let (kind, ttl): (String, i64) = pipe()
            .cmd("TYPE")
            .arg(key)
            .cmd("TTL")
            .arg(key)
            .query_async(&mut con)
            .await?;
        let mut detail = KeyDetail {
            key: key.to_string(),
            kind: kind.clone(),
            ttl: (ttl >= 0).then_some(ttl),
            ..Default::default()
        };
        if kind == "none" {
            return Ok(detail);
        }
        // Some managed servers refuse MEMORY; the size is only a nicety.
        detail.size = cmd("MEMORY")
            .arg("USAGE")
            .arg(key)
            .query_async::<Option<u64>>(&mut con)
            .await
            .ok()
            .flatten();
        let cap = KEY_ITEMS_CAP;
        let length_command = match kind.as_str() {
            "string" => "STRLEN",
            "hash" => "HLEN",
            "list" => "LLEN",
            "set" => "SCARD",
            "zset" => "ZCARD",
            "stream" => "XLEN",
            other => bail!("{other} keys can't be shown yet"),
        };
        detail.length = cmd(length_command).arg(key).query_async(&mut con).await?;
        detail.items = match kind.as_str() {
            "string" => {
                let text: Value = cmd("GETRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(STRING_CAP - 1)
                    .query_async(&mut con)
                    .await?;
                vec![(String::new(), strings(text).concat())]
            }
            "hash" => {
                let reply: Value = cmd("HSCAN")
                    .arg(key)
                    .arg(0)
                    .arg("COUNT")
                    .arg(cap)
                    .query_async(&mut con)
                    .await?;
                // [cursor, [field, value, …]]
                let mut flat = strings(reply);
                if !flat.is_empty() {
                    flat.remove(0);
                }
                pairs(flat).into_iter().take(cap).collect()
            }
            "list" => {
                let reply: Value = cmd("LRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(cap - 1)
                    .query_async(&mut con)
                    .await?;
                strings(reply)
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| (index.to_string(), value))
                    .collect()
            }
            "set" => {
                let reply: Value = cmd("SSCAN")
                    .arg(key)
                    .arg(0)
                    .arg("COUNT")
                    .arg(cap)
                    .query_async(&mut con)
                    .await?;
                let mut members = strings(reply);
                if !members.is_empty() {
                    members.remove(0);
                }
                members.sort();
                members
                    .into_iter()
                    .take(cap)
                    .map(|member| (member, String::new()))
                    .collect()
            }
            "zset" => {
                let reply: Value = cmd("ZRANGE")
                    .arg(key)
                    .arg(0)
                    .arg(cap - 1)
                    .arg("WITHSCORES")
                    .query_async(&mut con)
                    .await?;
                pairs(strings(reply))
            }
            _ => {
                let reply: Value = cmd("XRANGE")
                    .arg(key)
                    .arg("-")
                    .arg("+")
                    .arg("COUNT")
                    .arg(cap)
                    .query_async(&mut con)
                    .await?;
                let Value::Array(entries) = reply else {
                    bail!("unexpected XRANGE reply");
                };
                entries
                    .into_iter()
                    .filter_map(|entry| {
                        let Value::Array(mut parts) = entry else {
                            return None;
                        };
                        let fields = parts.pop().map(strings).unwrap_or_default();
                        let id = parts.pop().map(strings)?.concat();
                        let shown: Vec<String> = pairs(fields)
                            .into_iter()
                            .map(|(field, value)| format!("{field}={value}"))
                            .collect();
                        Some((id, shown.join(", ")))
                    })
                    .collect()
            }
        };
        detail.truncated = match kind.as_str() {
            "string" => detail.length as usize > STRING_CAP,
            _ => (detail.items.len() as u64) < detail.length,
        };
        Ok(detail)
    }

    /// Runs `edits` on `key` in one MULTI / EXEC.
    pub async fn apply_key_edits(&self, key: &str, edits: &[KeyEdit]) -> Result<()> {
        let mut con = self.con.clone();
        let mut transaction = pipe();
        transaction.atomic();
        for command in edit_commands(key, edits) {
            let (name, arguments) = command.split_first().expect("commands have a name");
            let step = transaction.cmd(name);
            for argument in arguments {
                step.arg(argument);
            }
            step.ignore();
        }
        let _: () = transaction.query_async(&mut con).await?;
        Ok(())
    }

    /// EXPIRE with `Some(seconds)`, PERSIST with `None`.
    pub async fn set_ttl(&self, key: &str, seconds: Option<i64>) -> Result<()> {
        let mut con = self.con.clone();
        match seconds {
            Some(seconds) if seconds > 0 => {
                let _: i64 = cmd("EXPIRE")
                    .arg(key)
                    .arg(seconds)
                    .query_async(&mut con)
                    .await?;
            }
            _ => {
                let _: i64 = cmd("PERSIST").arg(key).query_async(&mut con).await?;
            }
        }
        Ok(())
    }

    /// RENAMENX: never overwrites another key.
    pub async fn rename_key(&self, from: &str, to: &str) -> Result<()> {
        if to.is_empty() {
            bail!("keys can't be empty");
        }
        let mut con = self.con.clone();
        let renamed: i64 = cmd("RENAMENX")
            .arg(from)
            .arg(to)
            .query_async(&mut con)
            .await?;
        if renamed == 0 {
            bail!("key {to:?} already exists; nothing was renamed");
        }
        Ok(())
    }

    pub async fn delete_key(&self, key: &str) -> Result<()> {
        let mut con = self.con.clone();
        let _: i64 = cmd("DEL").arg(key).query_async(&mut con).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_removals_use_a_marker_after_the_sets() {
        let commands = edit_commands(
            "queue",
            &[
                KeyEdit::ListRemove(0),
                KeyEdit::ListPush("new".into()),
                KeyEdit::ListSet {
                    index: 2,
                    value: "changed".into(),
                },
                KeyEdit::ListRemove(3),
            ],
        );
        let names: Vec<&str> = commands.iter().map(|command| command[0].as_str()).collect();
        assert_eq!(names, ["LSET", "LSET", "LSET", "LREM", "RPUSH"]);
        assert_eq!(
            commands[0][3], commands[3][3],
            "marks and removes the same marker"
        );
        assert_eq!(commands[2][2], "3");
    }

    #[test]
    fn preview_quotes_arguments() {
        assert_eq!(
            preview(
                "user:1",
                &[KeyEdit::HashSet {
                    field: "name".into(),
                    value: "Ana Souza".into()
                }]
            ),
            "HSET \"user:1\" \"name\" \"Ana Souza\""
        );
    }
}
