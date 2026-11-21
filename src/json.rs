use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}

impl JsonValue {
    pub fn parse(input: &str) -> Result<JsonValue, String> {
        let mut parser = Parser::new(input);
        let val = parser.parse_value()?;
        parser.skip_whitespace();
        if parser.pos < parser.chars.len() {
            return Err(format!("Unexpected trailing characters at pos {}", parser.pos));
        }
        Ok(val)
    }

    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            JsonValue::Object(map) => map.get(key),
            _ => None,
        }
    }

    pub fn get_path(&self, path: &[&str]) -> Option<&JsonValue> {
        let mut cur = self;
        for key in path {
            cur = cur.get(key)?;
        }
        Some(cur)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            JsonValue::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            JsonValue::Number(n) => Some(*n as i64),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            JsonValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<JsonValue>> {
        match self {
            JsonValue::Array(arr) => Some(arr),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&BTreeMap<String, JsonValue>> {
        match self {
            JsonValue::Object(map) => Some(map),
            _ => None,
        }
    }

    pub fn to_string_pretty(&self) -> String {
        let mut out = String::new();
        self.write_pretty(&mut out, 0);
        out
    }

    fn write_pretty(&self, out: &mut String, indent: usize) {
        let pad = "  ".repeat(indent);
        let inner_pad = "  ".repeat(indent + 1);
        match self {
            JsonValue::Null => out.push_str("null"),
            JsonValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            JsonValue::Number(n) => {
                if n.is_finite() {
                    if n.fract() == 0.0 && n.abs() < 1e15 {
                        out.push_str(&format!("{:.0}", n));
                    } else {
                        out.push_str(&format!("{}", n));
                    }
                } else {
                    out.push_str("null");
                }
            }
            JsonValue::String(s) => write_escaped_string(out, s),
            JsonValue::Array(arr) => {
                if arr.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push_str("[\n");
                for (i, item) in arr.iter().enumerate() {
                    out.push_str(&inner_pad);
                    item.write_pretty(out, indent + 1);
                    if i + 1 < arr.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push(']');
            }
            JsonValue::Object(map) => {
                if map.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push_str("{\n");
                for (i, (k, v)) in map.iter().enumerate() {
                    out.push_str(&inner_pad);
                    write_escaped_string(out, k);
                    out.push_str(": ");
                    v.write_pretty(out, indent + 1);
                    if i + 1 < map.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push('}');
            }
        }
    }
}

impl fmt::Display for JsonValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        write_compact(self, &mut out);
        f.write_str(&out)
    }
}

fn write_compact(val: &JsonValue, out: &mut String) {
    match val {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        JsonValue::Number(n) => {
            if n.is_finite() {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    out.push_str(&format!("{:.0}", n));
                } else {
                    out.push_str(&format!("{}", n));
                }
            } else {
                out.push_str("null");
            }
        }
        JsonValue::String(s) => write_escaped_string(out, s),
        JsonValue::Array(arr) => {
            out.push('[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_compact(item, out);
            }
            out.push(']');
        }
        JsonValue::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_escaped_string(out, k);
                out.push(':');
                write_compact(v, out);
            }
            out.push('}');
        }
    }
}

pub fn write_escaped_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
        }
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += 1;
        Some(ch)
    }

    fn parse_value(&mut self) -> Result<JsonValue, String> {
        self.skip_whitespace();
        let ch = self
            .peek()
            .ok_or_else(|| "Unexpected end of JSON input".to_string())?;
        match ch {
            'n' => self.parse_null(),
            't' | 'f' => self.parse_bool(),
            '"' => self.parse_string().map(JsonValue::String),
            '[' => self.parse_array(),
            '{' => self.parse_object(),
            '-' | '0'..='9' => self.parse_number(),
            other => Err(format!("Unexpected character '{}' at pos {}", other, self.pos)),
        }
    }

    fn parse_null(&mut self) -> Result<JsonValue, String> {
        self.expect_keyword("null")?;
        Ok(JsonValue::Null)
    }

    fn parse_bool(&mut self) -> Result<JsonValue, String> {
        if self.peek() == Some('t') {
            self.expect_keyword("true")?;
            Ok(JsonValue::Bool(true))
        } else {
            self.expect_keyword("false")?;
            Ok(JsonValue::Bool(false))
        }
    }

    fn expect_keyword(&mut self, kw: &str) -> Result<(), String> {
        for expected in kw.chars() {
            match self.next() {
                Some(c) if c == expected => {}
                _ => return Err(format!("Expected keyword '{}' at pos {}", kw, self.pos)),
            }
        }
        Ok(())
    }

    fn parse_string(&mut self) -> Result<String, String> {
        if self.next() != Some('"') {
            return Err(format!("Expected '\"' at pos {}", self.pos));
        }
        let mut s = String::new();
        while let Some(ch) = self.next() {
            match ch {
                '"' => return Ok(s),
                '\\' => {
                    let esc = self
                        .next()
                        .ok_or_else(|| "Unexpected EOF in string escape".to_string())?;
                    match esc {
                        '"' => s.push('"'),
                        '\\' => s.push('\\'),
                        '/' => s.push('/'),
                        'b' => s.push('\u{08}'),
                        'f' => s.push('\u{0c}'),
                        'n' => s.push('\n'),
                        'r' => s.push('\r'),
                        't' => s.push('\t'),
                        'u' => {
                            let code = self.parse_hex4()?;
                            if (0xD800..=0xDBFF).contains(&code) {
                                // UTF-16 surrogate pair
                                if self.next() == Some('\\') && self.next() == Some('u') {
                                    let low = self.parse_hex4()?;
                                    let combined = 0x10000
                                        + (((code as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
                                    if let Some(c) = char::from_u32(combined) {
                                        s.push(c);
                                    }
                                }
                            } else if let Some(c) = char::from_u32(code as u32) {
                                s.push(c);
                            }
                        }
                        other => s.push(other),
                    }
                }
                other => s.push(other),
            }
        }
        Err("Unterminated JSON string".to_string())
    }

    fn parse_hex4(&mut self) -> Result<u16, String> {
        let mut hex = String::with_capacity(4);
        for _ in 0..4 {
            let c = self
                .next()
                .ok_or_else(|| "Unexpected EOF in \\u escape".to_string())?;
            hex.push(c);
        }
        u16::from_str_radix(&hex, 16).map_err(|e| format!("Invalid hex escape '{}': {}", hex, e))
    }

    fn parse_number(&mut self) -> Result<JsonValue, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-') {
                self.pos += 1;
            } else {
                break;
            }
        }
        let num_str: String = self.chars[start..self.pos].iter().collect();
        let val: f64 = num_str
            .parse()
            .map_err(|e| format!("Invalid number '{}': {}", num_str, e))?;
        Ok(JsonValue::Number(val))
    }

    fn parse_array(&mut self) -> Result<JsonValue, String> {
        self.next(); // '['
        self.skip_whitespace();
        let mut arr = Vec::new();
        if self.peek() == Some(']') {
            self.next();
            return Ok(JsonValue::Array(arr));
        }
        loop {
            let val = self.parse_value()?;
            arr.push(val);
            self.skip_whitespace();
            match self.next() {
                Some(',') => continue,
                Some(']') => return Ok(JsonValue::Array(arr)),
                other => {
                    return Err(format!(
                        "Expected ',' or ']' in array at pos {}, got {:?}",
                        self.pos, other
                    ))
                }
            }
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue, String> {
        self.next(); // '{'
        self.skip_whitespace();
        let mut map = BTreeMap::new();
        if self.peek() == Some('}') {
            self.next();
            return Ok(JsonValue::Object(map));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            if self.next() != Some(':') {
                return Err(format!("Expected ':' after object key at pos {}", self.pos));
            }
            let val = self.parse_value()?;
            map.insert(key, val);
            self.skip_whitespace();
            match self.next() {
                Some(',') => continue,
                Some('}') => return Ok(JsonValue::Object(map)),
                other => {
                    return Err(format!(
                        "Expected ',' or '}}' in object at pos {}, got {:?}",
                        self.pos, other
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_and_roundtrip() {
        let raw = r#"{
            "rate_limits": {
                "five_hour": {"used_percentage": 26.5, "resets_at": 1773990000},
                "seven_day": {"used_percentage": 2.0, "resets_at": 1774584000}
            },
            "ok": true,
            "items": [1, "two", null]
        }"#;
        let parsed = JsonValue::parse(raw).expect("valid json");
        let five_h = parsed
            .get_path(&["rate_limits", "five_hour", "used_percentage"])
            .and_then(|v| v.as_f64());
        assert_eq!(five_h, Some(26.5));
        let re_parsed = JsonValue::parse(&parsed.to_string()).expect("roundtrip");
        assert_eq!(parsed, re_parsed);
    }
}
