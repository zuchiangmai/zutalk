//! One event as it is kept: read from `tracing` into flat fields once, then written as a JSON
//! line or held by a capture. Whatever is not plain is turned away here, for both.

use crate::{Level, is_plain};
use tracing::field::{Field, Visit};

/// The names every line has before its own fields: a field that takes one is turned away.
const HEAD: [&str; 5] = ["t_ms", "seq", "level", "component", "event"];

/// What a turned-away value is written as.
const REFUSED: &str = "!";

/// A field's value, as it was said.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    U64(u64),
    I64(i64),
    F64(f64),
    Bool(bool),
    Text(String),
}

/// One event: its level, the module that said it, its code, and its fields in the order said.
#[derive(Clone, Debug)]
pub struct Line {
    pub level: Level,
    pub component: &'static str,
    pub code: &'static str,
    pub fields: Vec<(&'static str, Value)>,
}

impl Line {
    /// A field by its name.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.fields.iter().find(|(field, _)| *field == name).map(|(_, value)| value)
    }

    /// A field that is a number, as a `u64`.
    pub fn u64(&self, name: &str) -> Option<u64> {
        match self.get(name)? {
            Value::U64(n) => Some(*n),
            Value::I64(n) => u64::try_from(*n).ok(),
            _ => None,
        }
    }

    pub fn i64(&self, name: &str) -> Option<i64> {
        match self.get(name)? {
            Value::I64(n) => Some(*n),
            Value::U64(n) => i64::try_from(*n).ok(),
            _ => None,
        }
    }

    pub fn f64(&self, name: &str) -> Option<f64> {
        if let Value::F64(n) = self.get(name)? { Some(*n) } else { None }
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        if let Value::Bool(b) = self.get(name)? { Some(*b) } else { None }
    }

    pub fn text(&self, name: &str) -> Option<&str> {
        if let Value::Text(text) = self.get(name)? { Some(text) } else { None }
    }

    /// The names of its fields, in order.
    pub fn names(&self) -> Vec<&'static str> {
        self.fields.iter().map(|(name, _)| *name).collect()
    }

    /// The line a file holds: the head, then the fields, flat. No newline.
    pub(crate) fn json(&self, t_ms: u64, seq: u64) -> String {
        let mut out = String::with_capacity(LINE_GUESS_BYTES);
        let level = self.level.code();
        out.push_str(&format!(r#"{{"t_ms":{t_ms},"seq":{seq},"level":"{level}","component":"#));
        text(&mut out, self.component);
        out.push_str(r#","event":"#);
        text(&mut out, self.code);
        for (name, value) in &self.fields {
            out.push(',');
            text(&mut out, name);
            out.push(':');
            match value {
                Value::U64(n) => out.push_str(&n.to_string()),
                Value::I64(n) => out.push_str(&n.to_string()),
                Value::F64(n) => match serde_json::Number::from_f64(*n) {
                    Some(n) => out.push_str(&n.to_string()),
                    None => text(&mut out, REFUSED),
                },
                Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                Value::Text(t) => text(&mut out, t),
            }
        }
        out.push('}');
        out
    }
}

/// Room for a usual line without growing: most are under this.
const LINE_GUESS_BYTES: usize = 192;

fn text(out: &mut String, text: &str) {
    out.push_str(&serde_json::Value::from(text).to_string());
}

/// An event's fields, read once. What is not plain becomes `"!"` and is counted.
#[derive(Default)]
pub(crate) struct Fields {
    pub list: Vec<(&'static str, Value)>,
    pub refused: u64,
}

impl Fields {
    fn put(&mut self, field: &Field, value: Value) {
        if HEAD.contains(&field.name()) {
            self.refuse(field);
        } else {
            self.list.push((field.name(), value));
        }
    }

    fn refuse(&mut self, field: &Field) {
        self.list.push((field.name(), Value::Text(REFUSED.into())));
        self.refused += 1;
    }
}

impl Visit for Fields {
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, Value::U64(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, Value::I64(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if value.is_finite() { self.put(field, Value::F64(value)) } else { self.refuse(field) }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, Value::Bool(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if is_plain(value) {
            self.put(field, Value::Text(value.into()))
        } else {
            self.refuse(field)
        }
    }

    /// Only another library's events get here: ours are numbers, booleans and text.
    fn record_debug(&mut self, field: &Field, _: &dyn std::fmt::Debug) {
        self.refuse(field);
    }
}
