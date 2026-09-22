//! Helpers for `--json` and plain-text output.

use std::fmt;

/// Escapes a string for use inside a JSON string literal.
pub fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out
}

pub fn print_status(json: bool, field: &str, value: &str) {
    if json {
        println!("{{\"{}\":\"{}\"}}", escape_json(field), escape_json(value));
    } else {
        println!("{field}: {value}");
    }
}

/// Builder for the flat JSON objects printed in `--json` mode.
#[derive(Default)]
pub struct JsonObject {
    fields: Vec<String>,
}

impl JsonObject {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a string field; the value is escaped.
    pub fn str(mut self, key: &str, value: &str) -> Self {
        self.fields
            .push(format!("\"{key}\":\"{}\"", escape_json(value)));
        self
    }

    /// Adds a number or boolean field verbatim.
    pub fn raw(mut self, key: &str, value: impl fmt::Display) -> Self {
        self.fields.push(format!("\"{key}\":{value}"));
        self
    }

    pub fn object(mut self, key: &str, value: JsonObject) -> Self {
        self.fields.push(format!("\"{key}\":{value}"));
        self
    }
}

impl fmt::Display for JsonObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{{}}}", self.fields.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::escape_json;

    #[test]
    fn escape_json_handles_control_characters() {
        assert_eq!(escape_json("a\"b\\c"), "a\\\"b\\\\c");
        assert_eq!(escape_json("l1\r\nl2\t"), "l1\\r\\nl2\\t");
        assert_eq!(escape_json("\u{1}\u{1f}"), "\\u0001\\u001f");
        assert_eq!(escape_json("юникод"), "юникод");
    }
}
