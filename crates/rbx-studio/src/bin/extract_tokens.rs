//! Extract Roblox Foundation design tokens from the shipped Lua theme files
//! (`RbxDesignFoundations/<Theme>/<Mode>.lua`) into machine-readable JSON.
//!
//! Note: the shipped files deliberately stub `Color3.fromRGB` to always return
//! green so that naive execution yields no real colors. We therefore parse the
//! literal table textually instead of evaluating Lua.
//!
//! Usage:
//!   extract-tokens <input.lua> <output.json>

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::fs;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let input = args.first().context("usage: extract-tokens <input.lua> <output.json>")?;
    let output = args.get(1).context("usage: extract-tokens <input.lua> <output.json>")?;

    let src = fs::read_to_string(input).with_context(|| format!("reading {input}"))?;
    let tokens = extract(&src).with_context(|| format!("parsing {input}"))?;

    let json = serde_json::to_string_pretty(&tokens)?;
    fs::write(output, json)?;
    println!(
        "wrote {} ({} top-level groups)",
        output,
        tokens.as_object().map(|o| o.len()).unwrap_or(0)
    );
    Ok(())
}

fn extract(src: &str) -> Result<Value> {
    let anchor = src
        .find("return function(scale)")
        .context("could not find `return function(scale)`")?;
    let rest = &src[anchor..];
    let table_start = rest.find("return {").context("could not find token table")? + "return ".len();

    let mut parser = Parser::new(rest[table_start..].chars().collect());
    let value = parser.parse_table()?;
    parser.skip_ws();
    Ok(value)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn new(chars: Vec<char>) -> Self {
        Self { chars, pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn skip_ws(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.pos += 1;
                }
                Some('-') if self.chars.get(self.pos + 1) == Some(&'-') => {
                    // comment: --[[ ... ]] or -- to end of line
                    self.pos += 2;
                    if self.peek() == Some('[') && self.chars.get(self.pos + 1) == Some(&'[') {
                        self.pos += 2;
                        while self.pos < self.chars.len() {
                            if self.chars[self.pos] == ']' && self.chars.get(self.pos + 1) == Some(&']') {
                                self.pos += 2;
                                break;
                            }
                            self.pos += 1;
                        }
                    } else {
                        while let Some(c) = self.peek() {
                            self.pos += 1;
                            if c == '\n' {
                                break;
                            }
                        }
                    }
                }
                _ => break,
            }
        }
    }

    fn expect(&mut self, ch: char) -> Result<()> {
        self.skip_ws();
        match self.bump() {
            Some(c) if c == ch => Ok(()),
            other => bail!("expected {ch:?}, found {other:?} at {}", self.pos),
        }
    }

    fn parse_table(&mut self) -> Result<Value> {
        self.expect('{')?;
        let mut map = Map::new();
        let mut array: Vec<Value> = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some('}') => {
                    self.bump();
                    break;
                }
                Some(',') => {
                    // tolerate stray/trailing commas
                    self.bump();
                    continue;
                }
                None => bail!("unexpected end of input inside table"),
                _ => {}
            }

            // Try a keyed entry: `Ident = value`. Bare short identifiers that
            // are not followed by `=` are treated as injected noise (the shipped
            // token files contain at least one stray `{s`).
            if self.starts_ident() {
                let save = self.pos;
                let key = self.parse_ident()?;
                self.skip_ws();
                if self.peek() == Some('=') {
                    self.bump();
                    let value = self.parse_value()?;
                    map.insert(key, value);
                    self.skip_ws();
                    if self.peek() == Some(',') {
                        self.bump();
                    }
                    continue;
                }
                if key.len() <= 2 {
                    continue;
                }
                self.pos = save;
            }

            // Positional (array) entry.
            let value = self.parse_value()?;
            array.push(value);
            self.skip_ws();
            if self.peek() == Some(',') {
                self.bump();
            }
        }

        if map.is_empty() {
            Ok(Value::Array(array))
        } else {
            for (i, value) in array.into_iter().enumerate() {
                map.insert((i + 1).to_string(), value);
            }
            Ok(Value::Object(map))
        }
    }

    fn starts_ident(&self) -> bool {
        matches!(self.peek(), Some(c) if c.is_alphabetic() || c == '_')
    }

    fn parse_value(&mut self) -> Result<Value> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_table(),
            Some('"') => Ok(Value::String(self.parse_string()?)),
            Some(c) if c == '-' || c.is_ascii_digit() => Ok(json!(self.parse_numeric()?)),
            Some(c) if c.is_alphabetic() || c == '_' => self.parse_word(),
            other => bail!("unexpected value start {other:?} at {}", self.pos),
        }
    }

    fn parse_word(&mut self) -> Result<Value> {
        let word = self.parse_ident()?;
        self.skip_ws();

        if self.peek() == Some('(') {
            // function call
            self.bump();
            let mut args = Vec::new();
            self.skip_ws();
            if self.peek() != Some(')') {
                loop {
                    args.push(self.parse_value()?);
                    self.skip_ws();
                    if self.peek() == Some(',') {
                        self.bump();
                        continue;
                    }
                    break;
                }
            }
            self.expect(')')?;

            return match word.as_str() {
                "Color3.fromRGB" => {
                    let r = arg_num(&args, 0)?;
                    let g = arg_num(&args, 1)?;
                    let b = arg_num(&args, 2)?;
                    Ok(json!({ "$rgb": [r, g, b] }))
                }
                "Font.new" => {
                    let family = arg_str(&args, 0).unwrap_or_default();
                    let weight = arg_str(&args, 1).unwrap_or_default();
                    let style = arg_str(&args, 2).unwrap_or_default();
                    Ok(json!({ "$font": { "family": family, "weight": weight, "style": style } }))
                }
                other => Ok(json!({ "$call": other, "args": args })),
            };
        }

        match word.as_str() {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Ok(Value::String(word)),
        }
    }

    fn parse_numeric(&mut self) -> Result<f64> {
        let mut value = self.parse_number_literal()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('*') => {
                    self.bump();
                    value *= self.parse_factor()?;
                }
                Some('/') => {
                    self.bump();
                    value /= self.parse_factor()?;
                }
                _ => break,
            }
        }
        Ok(value)
    }

    fn parse_factor(&mut self) -> Result<f64> {
        self.skip_ws();
        match self.peek() {
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number_literal(),
            Some(c) if c.is_alphabetic() || c == '_' => {
                // symbolic factor such as `scale` — normalize to 1.0
                let _ = self.parse_ident()?;
                Ok(1.0)
            }
            other => bail!("expected numeric factor, found {other:?} at {}", self.pos),
        }
    }

    fn parse_number_literal(&mut self) -> Result<f64> {
        self.skip_ws();
        let start = self.pos;
        if self.peek() == Some('-') {
            self.bump();
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' {
                self.bump();
            } else {
                break;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>()
            .with_context(|| format!("invalid number {text:?}"))
    }

    fn parse_ident(&mut self) -> Result<String> {
        self.skip_ws();
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' || c == '.' {
                self.bump();
            } else {
                break;
            }
        }
        if self.pos == start {
            bail!("expected identifier at {}", self.pos);
        }
        Ok(self.chars[start..self.pos].iter().collect())
    }

    fn parse_string(&mut self) -> Result<String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.bump() {
                Some('"') => break,
                Some('\\') => match self.bump() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(c) => out.push(c),
                    None => bail!("unterminated escape"),
                },
                Some(c) => out.push(c),
                None => bail!("unterminated string"),
            }
        }
        Ok(out)
    }
}

fn arg_num(args: &[Value], i: usize) -> Result<f64> {
    args.get(i)
        .and_then(Value::as_f64)
        .with_context(|| format!("expected numeric argument #{i}"))
}

fn arg_str(args: &[Value], i: usize) -> Option<String> {
    match args.get(i)? {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}
