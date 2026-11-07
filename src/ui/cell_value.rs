//! Checks a cell's text against its column type while it's edited (design.md,
//! Cell editors #2): what's wrong, or a hint about how it will be stored.
//! Only what can be told without the server; the server has the last word
//! on Commit.

/// What the editor shows under the cell.
#[derive(Debug, Default, PartialEq)]
pub struct Check {
    /// Blocks saving (↵ / Tab) while set.
    pub error: Option<String>,
    /// Informational: a length counter, or how the value will be stored.
    pub hint: Option<String>,
}

/// `numeric(12,2)` → (12, 2); `varchar(120)` → (120, None).
fn params(ty: &str) -> Option<(u32, Option<u32>)> {
    let inner = ty.split_once('(')?.1.split_once(')')?.0;
    let mut it = inner.split(',').map(|p| p.trim().parse::<u32>());
    let a = it.next()?.ok()?;
    let b = match it.next() {
        Some(Ok(b)) => Some(b),
        Some(Err(_)) => return None,
        None => None,
    };
    Some((a, b))
}

fn base(ty: &str) -> String {
    let t = ty.trim().to_ascii_lowercase();
    let b = t.split('(').next().unwrap_or_default().trim().to_string();
    // "character varying" / "timestamp with time zone": keep the words.
    b.trim_end_matches(" unsigned").to_string()
}

/// Range of a whole-number type, if `ty` is one.
fn int_range(ty: &str) -> Option<(i128, i128)> {
    let t = ty.trim().to_ascii_lowercase();
    let unsigned = t.contains("unsigned");
    let b = base(ty);
    let bits = match b.as_str() {
        "tinyint" if !t.ends_with("(1)") => 8,
        "smallint" | "int2" | "smallserial" => 16,
        "mediumint" => 24,
        "int" | "integer" | "int4" | "serial" => 32,
        "bigint" | "int8" | "bigserial" => 64,
        _ => return None,
    };
    Some(if unsigned {
        (0, (1i128 << bits) - 1)
    } else {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    })
}

fn is_decimal_type(b: &str) -> bool {
    matches!(
        b,
        "numeric"
            | "decimal"
            | "real"
            | "float"
            | "float4"
            | "float8"
            | "double"
            | "double precision"
            | "money"
    )
}

/// Parses the 32 hex digits of a UUID in any common form (dashes, braces,
/// upper case) and returns the canonical lower-case, hyphenated form.
pub fn normalize_uuid(text: &str) -> Option<String> {
    let hex: String = text
        .trim()
        .trim_start_matches("urn:uuid:")
        .chars()
        .filter(|c| !matches!(c, '-' | '{' | '}'))
        .collect();
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let h = hex.to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

/// Checks `text` (what's in the editor) for a column of type `ty`. An empty
/// type (query results without a known table) checks nothing.
pub fn check(ty: &str, text: &str) -> Check {
    let b = base(ty);
    let trimmed = text.trim();
    let needs_value = || Check {
        error: Some("Empty. ⌥⌫ sets NULL".into()),
        hint: None,
    };
    if let Some((lo, hi)) = int_range(ty) {
        if trimmed.is_empty() {
            return needs_value();
        }
        return match trimmed.parse::<i128>() {
            Ok(n) if n < lo || n > hi => Check {
                error: Some(format!("Out of range for {b} ({lo} to {hi})")),
                hint: None,
            },
            Ok(_) => Check::default(),
            Err(_) => Check {
                error: Some("Not a whole number".into()),
                hint: None,
            },
        };
    }
    if is_decimal_type(&b) {
        if trimmed.is_empty() {
            return needs_value();
        }
        let Ok(n) = trimmed.parse::<f64>() else {
            return Check {
                error: Some("Not a number".into()),
                hint: None,
            };
        };
        // numeric(p,s): show the stored form, and catch too many digits.
        if let Some((p, Some(s))) =
            params(ty).filter(|_| matches!(b.as_str(), "numeric" | "decimal"))
        {
            let stored = format!("{n:.*}", s as usize);
            let int_digits = stored
                .trim_start_matches('-')
                .split('.')
                .next()
                .unwrap_or_default()
                .trim_start_matches('0')
                .len() as u32;
            if int_digits > p.saturating_sub(s) {
                return Check {
                    error: Some(format!(
                        "Too large for {b}({p},{s}): at most {} digits before the point",
                        p.saturating_sub(s)
                    )),
                    hint: None,
                };
            }
            return Check {
                error: None,
                hint: (stored != trimmed).then(|| format!("saved as {stored}")),
            };
        }
        return Check::default();
    }
    match b.as_str() {
        "json" | "jsonb" => {
            if trimmed.is_empty() {
                return needs_value();
            }
            match serde_json::from_str::<serde_json::Value>(trimmed) {
                Ok(_) => Check::default(),
                Err(e) => Check {
                    error: Some(format!("Invalid JSON: {e}")),
                    hint: None,
                },
            }
        }
        "uuid" => match normalize_uuid(trimmed) {
            Some(_) => Check::default(),
            None => Check {
                error: Some("Not a UUID (32 hex digits)".into()),
                hint: None,
            },
        },
        "bool" | "boolean" => match trimmed.to_ascii_lowercase().as_str() {
            "t" | "f" | "true" | "false" | "1" | "0" | "y" | "n" | "yes" | "no" | "on" | "off" => {
                Check::default()
            }
            _ => Check {
                error: Some("Use true or false".into()),
                hint: None,
            },
        },
        "varchar" | "character varying" | "nvarchar" | "char" | "character" | "nchar"
        | "bpchar" => {
            let Some((max, _)) = params(ty) else {
                return Check::default();
            };
            let n = text.chars().count() as u32;
            let fixed = matches!(b.as_str(), "char" | "character" | "nchar" | "bpchar");
            if n > max {
                Check {
                    error: Some(format!("{n} / {max}: too long for {b}({max})")),
                    hint: None,
                }
            } else if fixed && n < max {
                Check {
                    error: None,
                    hint: Some(format!("{n} / {max} · the server pads with spaces")),
                }
            } else {
                Check {
                    error: None,
                    hint: Some(format!("{n} / {max}")),
                }
            }
        }
        _ => Check::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(ty: &str, text: &str) -> Option<String> {
        check(ty, text).error
    }

    #[test]
    fn whole_numbers_and_ranges() {
        assert_eq!(err("int4", "42"), None);
        assert_eq!(err("integer", "-2147483648"), None);
        assert!(err("int4", "2147483648").unwrap().contains("Out of range"));
        assert!(err("smallint", "40000").is_some());
        assert_eq!(err("int unsigned", "4294967295"), None);
        assert!(err("int unsigned", "-1").is_some());
        assert_eq!(err("bigint", "9223372036854775807"), None);
        assert_eq!(err("int8", "1.5").as_deref(), Some("Not a whole number"));
        assert!(err("int4", "").unwrap().contains("NULL"));
        // tinyint(1) is a boolean, not a number.
        assert_eq!(err("tinyint(1)", "x"), None);
    }

    #[test]
    fn decimals_show_the_stored_form() {
        assert_eq!(
            check("numeric(12,2)", "118.4").hint.as_deref(),
            Some("saved as 118.40")
        );
        assert_eq!(check("numeric(12,2)", "118.40"), Check::default());
        assert!(
            err("numeric(5,2)", "1234.5")
                .unwrap()
                .contains("at most 3 digits")
        );
        assert_eq!(err("numeric(5,2)", "999.99"), None);
        assert_eq!(err("double precision", "1e10"), None);
        assert_eq!(err("real", "abc").as_deref(), Some("Not a number"));
    }

    #[test]
    fn text_lengths() {
        assert_eq!(check("varchar(5)", "abc").hint.as_deref(), Some("3 / 5"));
        assert!(
            err("character varying(3)", "abcd")
                .unwrap()
                .starts_with("4 / 3")
        );
        assert!(check("char(3)", "a").hint.unwrap().contains("pads"));
        // Counts characters, not bytes.
        assert_eq!(err("varchar(2)", "éé"), None);
        assert_eq!(check("text", "anything"), Check::default());
        assert_eq!(check("", "anything"), Check::default());
    }

    #[test]
    fn json_uuid_bool() {
        assert_eq!(err("jsonb", r#"{"a": 1}"#), None);
        assert!(err("json", "{a: 1}").unwrap().starts_with("Invalid JSON"));
        assert_eq!(
            normalize_uuid("{5F0C2A1E-8B7D-4C3E-9A51-0D8F3B2E7C41}").as_deref(),
            Some("5f0c2a1e-8b7d-4c3e-9a51-0d8f3b2e7c41")
        );
        assert_eq!(
            normalize_uuid("5f0c2a1e8b7d4c3e9a510d8f3b2e7c41").as_deref(),
            Some("5f0c2a1e-8b7d-4c3e-9a51-0d8f3b2e7c41")
        );
        assert!(err("uuid", "not-a-uuid").is_some());
        assert_eq!(err("boolean", "TRUE"), None);
        assert!(err("bool", "maybe").is_some());
    }
}
