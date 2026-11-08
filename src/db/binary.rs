//! Binary values as text (design.md cell editor #13): drivers read them as
//! hex (`\x48…` from Postgres, `0x48…` from MySQL / SQLite); Save / Load
//! file turn that into bytes and back.

use super::Driver;

/// bytea, blob and its sizes, binary, varbinary, SQL Server's image.
pub fn is_binary_type(data_type: &str) -> bool {
    let lower = data_type.trim().to_ascii_lowercase();
    let base = lower.split(['(', ' ']).next().unwrap_or_default();
    matches!(
        base,
        "bytea"
            | "blob"
            | "tinyblob"
            | "mediumblob"
            | "longblob"
            | "binary"
            | "varbinary"
            | "image"
    )
}

/// The hex digits of `\x…` / `0x…` text, when that's what it is.
pub fn hex_digits(text: &str) -> Option<&str> {
    let digits = text
        .strip_prefix("\\x")
        .or_else(|| text.strip_prefix("0x"))?;
    (digits.len() % 2 == 0 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(digits)
}

/// The bytes `\x…` / `0x…` text stands for.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let digits = hex_digits(text)?;
    (0..digits.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&digits[at..at + 2], 16).ok())
        .collect()
}

/// `bytes` as the driver shows binary values: `\x…` on Postgres, `0x…`
/// elsewhere.
pub fn encode(driver: Driver, bytes: &[u8]) -> String {
    let digits: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    match driver {
        Driver::Postgres => format!("\\x{digits}"),
        _ => format!("0x{digits}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let bytes = [0x89, b'P', b'N', b'G', 0];
        assert_eq!(encode(Driver::Postgres, &bytes), "\\x89504e4700");
        assert_eq!(encode(Driver::MySql, &bytes), "0x89504e4700");
        assert_eq!(decode("\\x89504e4700").unwrap(), bytes);
        assert_eq!(decode("0x89504E4700").unwrap(), bytes);
        assert_eq!(decode("plain text"), None);
        assert_eq!(decode("0x123"), None, "odd number of digits");
    }

    #[test]
    fn knows_binary_types() {
        assert!(is_binary_type("bytea"));
        assert!(is_binary_type("varbinary(16)"));
        assert!(is_binary_type("LONGBLOB"));
        assert!(!is_binary_type("text"));
    }
}
