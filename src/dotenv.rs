//! Parses `.env` files with forward-only variable expansion.
//!
//! - `KEY=VALUE` per line, optional `export ` prefix, `#` starts a comment.
//! - Unquoted values are trimmed; ` #` starts an inline comment.
//! - `'…'` values are literal and never expanded.
//! - `"…"` values support `\n \r \t \" \\ \$` escapes and may span lines.
//! - `${VAR}` and `$VAR` expand in unquoted and double-quoted values,
//!   resolved against the lookup (lower-priority layers) and lines above
//!   in the same file. Undefined variables expand to "" and are reported.
//!
//! dotenvy resolves references against the process environment rather than
//! a caller-supplied set and does not report undefined references, so hum
//! parses these files itself.

use std::collections::HashMap;

use anyhow::{Result, bail};

#[derive(Debug, PartialEq)]
pub struct Var {
    pub key: String,
    pub value: String,
    pub line: usize,
}

/// A `${VAR}` reference that resolved to nothing.
#[derive(Debug, PartialEq)]
pub struct Undefined {
    pub line: usize,
    pub name: String,
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub vars: Vec<Var>,
    pub undefined: Vec<Undefined>,
}

/// Parses `src`; `lookup` resolves variables from lower-priority layers.
pub fn parse(src: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<Parsed> {
    let mut p = Parser {
        src: src.as_bytes(),
        pos: 0,
        line: 1,
        lookup: &lookup,
        local: HashMap::new(),
        out: Parsed::default(),
    };
    p.run()?;
    Ok(p.out)
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
    lookup: &'a dyn Fn(&str) -> Option<String>,
    local: HashMap<String, String>,
    out: Parsed,
}

fn is_name_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

fn is_name_char(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn skip_inline(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    fn skip_to_eol(&mut self) {
        while self.peek().is_some_and(|c| c != b'\n') {
            self.pos += 1;
        }
    }

    fn read_key(&mut self) -> String {
        let start = self.pos;
        if self.peek().is_some_and(is_name_start) {
            while self.peek().is_some_and(|c| is_name_char(c) || c == b'.') {
                self.pos += 1;
            }
        }
        String::from_utf8_lossy(&self.src[start..self.pos]).into_owned()
    }

    fn run(&mut self) -> Result<()> {
        loop {
            // Blank lines and comments.
            while let Some(c) = self.peek() {
                match c {
                    b' ' | b'\t' | b'\r' => self.pos += 1,
                    b'\n' => {
                        self.pos += 1;
                        self.line += 1;
                    }
                    b'#' => self.skip_to_eol(),
                    _ => break,
                }
            }
            if self.peek().is_none() {
                return Ok(());
            }
            let line = self.line;
            let mut key = self.read_key();
            if key == "export" && matches!(self.peek(), Some(b' ' | b'\t')) {
                self.skip_inline();
                key = self.read_key();
            }
            if key.is_empty() {
                bail!("line {line}: expected a variable name");
            }
            self.skip_inline();
            if self.peek() != Some(b'=') {
                bail!("line {line}: expected '=' after {key}");
            }
            self.pos += 1;
            self.skip_inline();
            let value = self.read_value(line)?;
            // Rest of the line: whitespace and an optional comment.
            self.skip_inline();
            if self.peek() == Some(b'\r') {
                self.pos += 1;
            }
            match self.peek() {
                None | Some(b'\n') => {}
                Some(b'#') => self.skip_to_eol(),
                _ => bail!("line {}: unexpected text after value", self.line),
            }
            self.local.insert(key.clone(), value.clone());
            self.out.vars.push(Var { key, value, line });
        }
    }

    fn read_value(&mut self, line: usize) -> Result<String> {
        let mut out = Vec::new();
        match self.peek() {
            Some(b'\'') => {
                self.pos += 1;
                loop {
                    match self.peek() {
                        None => bail!("line {line}: unterminated single-quoted value"),
                        Some(b'\'') => break,
                        Some(c) => {
                            self.line += usize::from(c == b'\n');
                            out.push(c);
                            self.pos += 1;
                        }
                    }
                }
                self.pos += 1;
            }
            Some(b'"') => {
                self.pos += 1;
                loop {
                    match self.peek() {
                        None => bail!("line {line}: unterminated double-quoted value"),
                        Some(b'"') => break,
                        Some(b'\\') if self.pos + 1 < self.src.len() => {
                            let e = self.src[self.pos + 1];
                            self.pos += 2;
                            self.line += usize::from(e == b'\n');
                            out.push(match e {
                                b'n' => b'\n',
                                b'r' => b'\r',
                                b't' => b'\t',
                                other => other, // \" \\ \$ and anything else
                            });
                        }
                        Some(b'$') => self.expand(&mut out, line),
                        Some(c) => {
                            self.line += usize::from(c == b'\n');
                            out.push(c);
                            self.pos += 1;
                        }
                    }
                }
                self.pos += 1;
            }
            _ => {
                // Unquoted: to end of line; " #" starts a comment.
                let start = self.pos;
                let mut end = start;
                while end < self.src.len() && self.src[end] != b'\n' {
                    if self.src[end] == b'#'
                        && end > start
                        && matches!(self.src[end - 1], b' ' | b'\t')
                    {
                        break;
                    }
                    end += 1;
                }
                while end > start && matches!(self.src[end - 1], b' ' | b'\t' | b'\r') {
                    end -= 1;
                }
                while self.pos < end {
                    match self.src[self.pos] {
                        b'\\' if self.src.get(self.pos + 1) == Some(&b'$') => {
                            out.push(b'$');
                            self.pos += 2;
                        }
                        b'$' => self.expand_until(&mut out, line, end),
                        c => {
                            out.push(c);
                            self.pos += 1;
                        }
                    }
                }
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn expand(&mut self, out: &mut Vec<u8>, line: usize) {
        self.expand_until(out, line, self.src.len());
    }

    /// Handles a `$` at `pos`: writes the expansion, or the literal `$` when
    /// no valid reference follows. Never reads at or past `end`.
    fn expand_until(&mut self, out: &mut Vec<u8>, line: usize, end: usize) {
        self.pos += 1;
        let src = &self.src[..end];
        if src.get(self.pos) == Some(&b'{') {
            let mut close = self.pos + 1;
            while close < end && is_name_char(src[close]) {
                close += 1;
            }
            let name = &src[self.pos + 1..close];
            if src.get(close) == Some(&b'}') && name.first().is_some_and(|&c| is_name_start(c)) {
                let name = String::from_utf8_lossy(name).into_owned();
                self.pos = close + 1;
                out.extend(self.resolve(name, line).bytes());
                return;
            }
        } else if src.get(self.pos).is_some_and(|&c| is_name_start(c)) {
            let start = self.pos;
            while src.get(self.pos).is_some_and(|&c| is_name_char(c)) {
                self.pos += 1;
            }
            let name = String::from_utf8_lossy(&src[start..self.pos]).into_owned();
            out.extend(self.resolve(name, line).bytes());
            return;
        }
        out.push(b'$');
    }

    fn resolve(&mut self, name: String, line: usize) -> String {
        if let Some(v) = self
            .local
            .get(&name)
            .cloned()
            .or_else(|| (self.lookup)(&name))
        {
            return v;
        }
        self.out.undefined.push(Undefined { line, name });
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_map(src: &str, lower: &[(&str, &str)]) -> (HashMap<String, String>, Parsed) {
        let lower: HashMap<String, String> = lower
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let p = parse(src, |k| lower.get(k).cloned()).unwrap();
        let m = p
            .vars
            .iter()
            .map(|v| (v.key.clone(), v.value.clone()))
            .collect();
        (m, p)
    }

    #[test]
    fn value_forms() {
        let src = "# comment\nPLAIN=hello world   # inline comment\nexport EXPORTED=1\nSINGLE='no $HOME expansion # kept'\nDOUBLE=\"line1\\nline2 \\\"q\\\" \\$HOME\"\nHASH=a#b\nEMPTY=\nSPACED = value\nMULTI=\"a\nb\"\n";
        let (m, _) = parse_map(src, &[("HOME", "/h")]);
        for (k, v) in [
            ("PLAIN", "hello world"),
            ("EXPORTED", "1"),
            ("SINGLE", "no $HOME expansion # kept"),
            ("DOUBLE", "line1\nline2 \"q\" $HOME"),
            ("HASH", "a#b"),
            ("EMPTY", ""),
            ("SPACED", "value"),
            ("MULTI", "a\nb"),
        ] {
            assert_eq!(m[k], v, "{k}");
        }
    }

    // Requirement: 变量展开, 只向前引用, 非引用的 `$` 保持原样
    #[test]
    fn expansion_is_forward_only() {
        let src = "DATA_DIR=${HOME}/data\nURL=\"http://$HOST:${PORT}/x\"\nPORT=8080\nLATE=${PORT}\nESC=\\$HOME\nLOWER=$lower_var\nDOLLAR=cost $5 and $ alone and ${unterminated\n";
        let (m, p) = parse_map(
            src,
            &[
                ("HOME", "/Users/me"),
                ("HOST", "localhost"),
                ("lower_var", "ok"),
            ],
        );
        for (k, v) in [
            ("DATA_DIR", "/Users/me/data"),
            ("URL", "http://localhost:/x"),
            ("LATE", "8080"),
            ("ESC", "$HOME"),
            ("LOWER", "ok"),
            ("DOLLAR", "cost $5 and $ alone and ${unterminated"),
        ] {
            assert_eq!(m[k], v, "{k}");
        }
        assert_eq!(
            p.undefined,
            [Undefined {
                line: 2,
                name: "PORT".into()
            }]
        );
    }

    #[test]
    fn same_file_shadows_lower_layers() {
        let (m, _) = parse_map("PATH=/bin\nP2=$PATH:/x\n", &[("PATH", "/usr/bin")]);
        assert_eq!(m["P2"], "/bin:/x");
    }

    #[test]
    fn invalid_files() {
        for src in [
            "NOEQUALS\n",
            "A='unterminated\n",
            "A=\"unterminated\n",
            "1A=x\n",
            "A='x' junk\n",
        ] {
            assert!(parse(src, |_| None).is_err(), "{src:?}");
        }
    }

    #[test]
    fn crlf() {
        let (m, _) = parse_map("A=1\r\nB=\"2\"\r\n", &[]);
        assert_eq!((m["A"].as_str(), m["B"].as_str()), ("1", "2"));
    }
}
