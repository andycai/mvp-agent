//! 极简 JSON 解析/序列化(仅用标准库)。
//!
//! Rust 标准库不含 JSON,这里手写一个小实现,要求能正确处理:
//! 字符串转义(\\" \\\\ \\/ \\b \\f \\n \\r \\t \\uXXXX 及 UTF-16 代理对)、
//! Unicode、整数/浮点数、嵌套对象与数组。
//! 序列化等价于 Python 的 \`json.dumps(..., ensure_ascii=False)\`:
//! 分隔符为 \`", "\` 与 \`": "\`,非 ASCII 原样输出(UTF-8)。

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// 取对象字段(数组/其它类型返回 None)。
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// 与 Python \`type(v).__name__\` 对齐的类型名,用于复刻错误文案。
    pub fn py_type_name(&self) -> &'static str {
        match self {
            Json::Null => "NoneType",
            Json::Bool(_) => "bool",
            Json::Int(_) => "int",
            Json::Num(_) => "float",
            Json::Str(_) => "str",
            Json::Arr(_) => "list",
            Json::Obj(_) => "dict",
        }
    }
}

/// 解析一段 JSON 文本;失败时返回形如 Python \`json.JSONDecodeError\` 的说明。
pub fn parse(s: &str) -> Result<Json, String> {
    let mut p = Parser { s, i: 0 };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    if p.i != p.s.len() {
        return Err(p.err("Extra data"));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.i).copied()
    }

    fn err(&self, msg: &str) -> String {
        let prefix = &self.s[..self.i.min(self.s.len())];
        let line = prefix.chars().filter(|c| *c == '\n').count() + 1;
        let col = prefix.chars().rev().take_while(|c| *c != '\n').count() + 1;
        format!(
            "{}: line {} column {} (char {})",
            msg,
            line,
            col,
            prefix.chars().count()
        )
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn lit(&mut self, word: &str) -> Result<(), String> {
        if self.s[self.i..].starts_with(word) {
            self.i += word.len();
            Ok(())
        } else {
            Err(self.err("Expecting value"))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek() {
            Some(b'n') => {
                self.lit("null")?;
                Ok(Json::Null)
            }
            Some(b't') => {
                self.lit("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.lit("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'"') => Ok(Json::Str(self.parse_string()?)),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_object(),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.parse_number(),
            _ => Err(self.err("Expecting value")),
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        // 调用时 s[i] == '"'
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => return Err(self.err("Unterminated string starting at")),
            };
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let e = match self.peek() {
                        Some(e) => e,
                        None => return Err(self.err("Unterminated string starting at")),
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let ch = if (0xD800..=0xDBFF).contains(&hi) {
                                // UTF-16 代理对:必须紧跟一个低位代理
                                if self.peek() == Some(b'\\')
                                    && self.s.as_bytes().get(self.i + 1) == Some(&b'u')
                                {
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if (0xDC00..=0xDFFF).contains(&lo) {
                                        let cp = 0x10000
                                            + ((hi as u32 - 0xD800) << 10)
                                            + (lo as u32 - 0xDC00);
                                        char::from_u32(cp).unwrap_or('\u{FFFD}')
                                    } else {
                                        '\u{FFFD}'
                                    }
                                } else {
                                    '\u{FFFD}'
                                }
                            } else if (0xDC00..=0xDFFF).contains(&hi) {
                                '\u{FFFD}'
                            } else {
                                char::from_u32(hi as u32).unwrap_or('\u{FFFD}')
                            };
                            out.push(ch);
                        }
                        _ => return Err(self.err("Invalid \\escape")),
                    }
                }
                _ => {
                    if c < 0x20 {
                        return Err(self.err("Invalid control character at"));
                    }
                    let start = self.i;
                    let ch = self.s[start..]
                        .chars()
                        .next()
                        .ok_or_else(|| self.err("Unterminated string starting at"))?;
                    self.i += ch.len_utf8();
                    out.push(ch);
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, String> {
        let mut v: u16 = 0;
        for _ in 0..4 {
            let c = self.peek().ok_or_else(|| self.err("Invalid \\uXXXX escape"))?;
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err(self.err("Invalid \\uXXXX escape")),
            };
            v = v * 16 + u16::from(d);
            self.i += 1;
        }
        Ok(v)
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.i += 1; // '['
        let mut arr = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(arr));
        }
        loop {
            arr.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                    self.skip_ws();
                }
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(arr));
                }
                _ => return Err(self.err("Expecting ',' delimiter")),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.i += 1; // '{'
        let mut obj: Vec<(String, Json)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(obj));
        }
        loop {
            if self.peek() != Some(b'"') {
                return Err(self.err("Expecting property name enclosed in double quotes"));
            }
            let k = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.err("Expecting ':' delimiter"));
            }
            self.i += 1;
            let v = self.value()?;
            obj.push((k, v));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                    self.skip_ws();
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(obj));
                }
                _ => return Err(self.err("Expecting ',' delimiter")),
            }
        }
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        let d0 = self.i;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.i == d0 {
            return Err(self.err("Expecting value"));
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            self.i += 1;
            is_float = true;
            let d1 = self.i;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
            if self.i == d1 {
                return Err(self.err("Expecting value"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            is_float = true;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let d2 = self.i;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
            if self.i == d2 {
                return Err(self.err("Expecting value"));
            }
        }
        let text = &self.s[start..self.i];
        if !is_float {
            if let Ok(n) = text.parse::<i64>() {
                return Ok(Json::Int(n));
            }
        }
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| self.err("Expecting value"))
    }
}

impl fmt::Display for Json {
    /// 紧凑序列化,分隔符与 Python \`json.dumps\` 一致(\`", "\` / \`": "\`)。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Json::Null => f.write_str("null"),
            Json::Bool(true) => f.write_str("true"),
            Json::Bool(false) => f.write_str("false"),
            Json::Int(n) => write!(f, "{}", n),
            Json::Num(x) => f.write_str(&fmt_float(*x)),
            Json::Str(s) => write_escaped(f, s),
            Json::Arr(a) => {
                f.write_str("[")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                f.write_str("]")
            }
            Json::Obj(o) => {
                f.write_str("{")?;
                for (i, (k, v)) in o.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write_escaped(f, k)?;
                    f.write_str(": ")?;
                    write!(f, "{}", v)?;
                }
                f.write_str("}")
            }
        }
    }
}

fn fmt_float(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    format!("{:?}", x)
}

fn write_escaped(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    f.write_str("\"")?;
    for c in s.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            '\u{8}' => f.write_str("\\b")?,
            '\u{c}' => f.write_str("\\f")?,
            c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
            c => write!(f, "{}", c)?,
        }
    }
    f.write_str("\"")
}
