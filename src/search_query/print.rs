// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! A query written back as text, the way a saved rule keeps it: names in
//! lowercase, `=` for `==`, `-field:=x` for `field:!=x`, quotes only where
//! they're needed, parentheses only where OR needs them, and the
//! directives at the end. Reading it gives the same query again

use itertools::Itertools;

use super::{Directives, Limit, Node, SortKey, lexer, parser::RESERVED};

pub(super) fn query(root: &Node, directives: &Directives) -> String {
    let mut parts = Vec::new();
    let text = node(root);
    if !text.is_empty() {
        parts.push(text);
    }
    for (name, keys) in [("sort", &directives.sort), ("pick", &directives.pick)] {
        if !keys.is_empty() {
            parts.push(format!("{name}:{}", keys.iter().map(sort_key).join(",")));
        }
    }
    if let Some(limit) = directives.limit {
        let limit = match limit {
            Limit::Count(count) => count.to_string(),
            Limit::Duration(seconds) => duration(seconds),
        };
        parts.push(format!("limit:{limit}"));
    }
    if let Some(by) = directives.by {
        parts.push(format!("by:{}", by.name()));
    }
    parts.join(" ")
}

fn node(node: &Node) -> String {
    match node {
        Node::All => String::new(),
        Node::Word(word) => {
            // Text that would read as syntax is quoted
            let syntax =
                word.contains(':') || word.starts_with(['-', '<', '>', '!']) || needs_quotes(word);
            if syntax { quoted(word) } else { word.clone() }
        }
        Node::Term(term) => {
            let raw = if term.raw.starts_with(['=', '<', '>', '!', ':']) || needs_quotes(&term.raw)
            {
                quoted(&term.raw)
            } else {
                term.raw.clone()
            };
            format!("{}:{}{raw}", term.field.name(), term.op.text())
        }
        Node::Has(field) => format!("has:{}", field.name()),
        Node::Is(flag) => format!("is:{}", flag.name()),
        Node::Not(inner) => match **inner {
            Node::And(_) | Node::Or(_) => format!("-({})", self::node(inner)),
            _ => format!("-{}", self::node(inner)),
        },
        Node::And(nodes) => nodes
            .iter()
            .map(|inner| match inner {
                Node::Or(_) => format!("({})", self::node(inner)),
                _ => self::node(inner),
            })
            .join(" "),
        Node::Or(nodes) => nodes.iter().map(self::node).join(" OR "),
    }
}

// Whether text has what would end it, or what a rule must quote
fn needs_quotes(text: &str) -> bool {
    text.is_empty()
        || text.chars().any(|c| {
            c.is_whitespace() || matches!(c, '(' | ')' | '"' | '\\') || RESERVED.contains(&c)
        })
        || lexer::keyword(text).is_some()
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn sort_key(key: &SortKey) -> String {
    let sign = if key.descending { "-" } else { "" };
    format!("{sign}{}", key.by.name())
}

// A length as `limit:` takes it, e.g. "1h30min"
fn duration(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    let mut text = String::new();
    if hours > 0 {
        text.push_str(&format!("{hours}h"));
    }
    if minutes > 0 {
        text.push_str(&format!("{minutes}min"));
    }
    if seconds > 0 || text.is_empty() {
        text.push_str(&format!("{seconds}s"));
    }
    text
}
