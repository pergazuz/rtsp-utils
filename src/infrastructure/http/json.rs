//! A small JSON encoder and decoder.
//!
//! The control API exchanges a handful of flat shapes, so a full serialisation
//! framework would be a heavier dependency than the problem deserves.

use std::fmt::Write as _;

use crate::domain::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn object(fields: Vec<(&str, Json)>) -> Json {
        Json::Object(
            fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    pub fn string(value: impl Into<String>) -> Json {
        Json::String(value.into())
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn to_json_string(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Number(n) => {
                // JSON has no way to spell a non-finite number.
                if n.is_finite() {
                    let _ = write!(out, "{n}");
                } else {
                    out.push_str("null");
                }
            }
            Json::String(s) => write_string(s, out),
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Object(fields) => {
                out.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_string(value: &str, out: &mut String) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Control characters have to be escaped; everything else, including
            // non-ASCII, can go out as UTF-8.
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn parse(input: &str) -> Result<Json> {
    let bytes = input.as_bytes();
    let mut pos = 0usize;
    let value = parse_value(bytes, &mut pos)?;
    skip_whitespace(bytes, &mut pos);
    if pos != bytes.len() {
        return Err(malformed("trailing characters after the JSON value"));
    }
    Ok(value)
}

fn parse_value(b: &[u8], pos: &mut usize) -> Result<Json> {
    skip_whitespace(b, pos);
    match b.get(*pos) {
        None => Err(malformed("unexpected end of input")),
        Some(b'n') => expect(b, pos, "null").map(|_| Json::Null),
        Some(b't') => expect(b, pos, "true").map(|_| Json::Bool(true)),
        Some(b'f') => expect(b, pos, "false").map(|_| Json::Bool(false)),
        Some(b'"') => parse_string(b, pos).map(Json::String),
        Some(b'[') => parse_array(b, pos),
        Some(b'{') => parse_object(b, pos),
        Some(_) => parse_number(b, pos),
    }
}

fn parse_array(b: &[u8], pos: &mut usize) -> Result<Json> {
    *pos += 1; // '['
    let mut items = Vec::new();
    skip_whitespace(b, pos);
    if b.get(*pos) == Some(&b']') {
        *pos += 1;
        return Ok(Json::Array(items));
    }
    loop {
        items.push(parse_value(b, pos)?);
        skip_whitespace(b, pos);
        match b.get(*pos) {
            Some(b',') => *pos += 1,
            Some(b']') => {
                *pos += 1;
                return Ok(Json::Array(items));
            }
            _ => return Err(malformed("expected ',' or ']' in array")),
        }
    }
}

fn parse_object(b: &[u8], pos: &mut usize) -> Result<Json> {
    *pos += 1; // '{'
    let mut fields = Vec::new();
    skip_whitespace(b, pos);
    if b.get(*pos) == Some(&b'}') {
        *pos += 1;
        return Ok(Json::Object(fields));
    }
    loop {
        skip_whitespace(b, pos);
        let key = parse_string(b, pos)?;
        skip_whitespace(b, pos);
        if b.get(*pos) != Some(&b':') {
            return Err(malformed("expected ':' after an object key"));
        }
        *pos += 1;
        fields.push((key, parse_value(b, pos)?));
        skip_whitespace(b, pos);
        match b.get(*pos) {
            Some(b',') => *pos += 1,
            Some(b'}') => {
                *pos += 1;
                return Ok(Json::Object(fields));
            }
            _ => return Err(malformed("expected ',' or '}' in object")),
        }
    }
}

fn parse_string(b: &[u8], pos: &mut usize) -> Result<String> {
    if b.get(*pos) != Some(&b'"') {
        return Err(malformed("expected a string"));
    }
    *pos += 1;

    let mut out = String::new();
    loop {
        let byte = *b.get(*pos).ok_or_else(|| malformed("unterminated string"))?;
        *pos += 1;
        match byte {
            b'"' => return Ok(out),
            b'\\' => {
                let escape = *b
                    .get(*pos)
                    .ok_or_else(|| malformed("unterminated escape sequence"))?;
                *pos += 1;
                match escape {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let hex = b
                            .get(*pos..*pos + 4)
                            .ok_or_else(|| malformed("truncated \\u escape"))?;
                        let code = u32::from_str_radix(
                            std::str::from_utf8(hex).map_err(|_| malformed("bad \\u escape"))?,
                            16,
                        )
                        .map_err(|_| malformed("bad \\u escape"))?;
                        *pos += 4;
                        // Lone surrogates cannot be represented; substitute
                        // rather than reject the whole document.
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    _ => return Err(malformed("unknown escape sequence")),
                }
            }
            // Multi-byte UTF-8 passes through untouched.
            _ => {
                let start = *pos - 1;
                let len = utf8_len(byte);
                let slice = b
                    .get(start..start + len)
                    .ok_or_else(|| malformed("truncated UTF-8 sequence"))?;
                out.push_str(std::str::from_utf8(slice).map_err(|_| malformed("invalid UTF-8"))?);
                *pos = start + len;
            }
        }
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

fn parse_number(b: &[u8], pos: &mut usize) -> Result<Json> {
    let start = *pos;
    while let Some(c) = b.get(*pos) {
        if matches!(c, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
            *pos += 1;
        } else {
            break;
        }
    }
    std::str::from_utf8(&b[start..*pos])
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .map(Json::Number)
        .ok_or_else(|| malformed("invalid number"))
}

fn expect(b: &[u8], pos: &mut usize, literal: &str) -> Result<()> {
    if b.get(*pos..*pos + literal.len()) == Some(literal.as_bytes()) {
        *pos += literal.len();
        Ok(())
    } else {
        Err(malformed(&format!("expected '{literal}'")))
    }
}

fn skip_whitespace(b: &[u8], pos: &mut usize) {
    while matches!(b.get(*pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        *pos += 1;
    }
}

fn malformed(message: &str) -> Error {
    Error::Protocol(format!("malformed JSON: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_objects_and_arrays() {
        let value = Json::object(vec![
            ("name", Json::string("91")),
            ("active", Json::Bool(true)),
            ("viewers", Json::Number(2.0)),
            ("tracks", Json::Array(vec![Json::string("video")])),
            ("startedAt", Json::Null),
        ]);
        assert_eq!(
            value.to_json_string(),
            r#"{"name":"91","active":true,"viewers":2,"tracks":["video"],"startedAt":null}"#
        );
    }

    #[test]
    fn escapes_strings() {
        let value = Json::string("a\"b\\c\nd\te\u{1}f");
        assert_eq!(value.to_json_string(), r#""a\"b\\c\nd\te\u0001f""#);
    }

    #[test]
    fn keeps_non_ascii_as_utf8() {
        assert_eq!(Json::string("café 日本").to_json_string(), "\"café 日本\"");
    }

    #[test]
    fn non_finite_numbers_degrade_to_null() {
        assert_eq!(Json::Number(f64::NAN).to_json_string(), "null");
        assert_eq!(Json::Number(f64::INFINITY).to_json_string(), "null");
    }

    #[test]
    fn parses_the_shapes_the_api_accepts() {
        let value = parse(r#"{"path":"C:\\media\\91.mov","name":"91","start":false}"#).unwrap();
        assert_eq!(value.get("path").unwrap().as_str(), Some("C:\\media\\91.mov"));
        assert_eq!(value.get("name").unwrap().as_str(), Some("91"));
        assert_eq!(value.get("start").unwrap().as_bool(), Some(false));
        assert!(value.get("missing").is_none());
    }

    #[test]
    fn round_trips_nested_values() {
        // Numbers come back in their canonical form, so `-3e2` re-serialises
        // as `-300`; everything else survives verbatim.
        let text = r#"{"a":[1,2.5,-3e2],"b":{"c":null},"d":"x"}"#;
        let once = parse(text).unwrap();
        assert_eq!(
            once.to_json_string(),
            r#"{"a":[1,2.5,-300],"b":{"c":null},"d":"x"}"#
        );
        // Re-parsing the output is a fixed point, which is what callers rely on.
        assert_eq!(parse(&once.to_json_string()).unwrap(), once);
    }

    #[test]
    fn parses_escapes_including_unicode() {
        let value = parse(r#"{"s":"line\nbreak \u00e9 \"quoted\""}"#).unwrap();
        assert_eq!(
            value.get("s").unwrap().as_str(),
            Some("line\nbreak é \"quoted\"")
        );
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse("").is_err());
        assert!(parse("{").is_err());
        assert!(parse(r#"{"a" 1}"#).is_err());
        assert!(parse(r#"{"a":1}trailing"#).is_err());
        assert!(parse(r#""unterminated"#).is_err());
    }
}
