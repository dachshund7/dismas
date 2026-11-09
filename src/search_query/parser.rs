// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reads the tokens of a search into a query:
//!
//! ```text
//! query  := or                 or := and ('OR' and)*
//! and    := unary ('AND'? unary)*
//! unary  := ('-' | 'NOT') unary | '(' or ')'? | term
//! term   := field ':' op? value | field ':' '(' or ')'? | ('is' | 'has') ':' name
//!         | directive ':' value | value
//! ```
//!
//! In a field's parentheses, each bare value takes the field. NOT binds
//! tighter than AND, and AND than OR, as in Strawberry.

use std::ops::Range;

use super::{
    Directive, Directives, Field, IsFlag, Kind, Limit, MAX_DEPTH, Mode, Node, Op, Problem,
    ProblemKind, Query, SortBy, SortKey, Term, Test, Unit,
    lexer::{Spanned, Token, Word, tokens},
    values,
};
use crate::audio::normalize_search_text;

// The characters a rule must quote, for wildcards later
pub(super) const RESERVED: [char; 5] = ['*', '?', '@', '[', ']'];

// What a name before a colon is: a field, `is:` or `has:`, or a directive
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Name {
    Field(Field),
    Is,
    Has,
    Directive(Directive),
}

impl Name {
    // The name, ignoring ASCII case
    pub(super) fn parse(name: &str) -> Option<Self> {
        if let Some(field) = Field::from_name(name) {
            Some(Self::Field(field))
        } else if name.eq_ignore_ascii_case("is") {
            Some(Self::Is)
        } else if name.eq_ignore_ascii_case("has") {
            Some(Self::Has)
        } else {
            Directive::from_name(name).map(Self::Directive)
        }
    }

    // All the names the syntax knows
    pub(super) fn all() -> impl Iterator<Item = &'static str> {
        Field::ALL
            .iter()
            .map(|field| field.name())
            .chain(["is", "has"])
            .chain(Directive::ALL.iter().map(|directive| directive.name()))
    }
}

pub(super) fn parse(text: &str, mode: Mode) -> Query {
    let mut parser = Parser {
        tokens: tokens(text),
        pos: 0,
        depth: 0,
        mode,
        problems: Vec::new(),
        directives: Directives::default(),
    };
    let mut nodes = Vec::new();
    loop {
        nodes.push(parser.or(None));
        // A stray ")" is ignored
        match parser.peek() {
            Some(Token::Close) => {
                let span = parser.span();
                parser.pos += 1;
                parser.rule_problem(span, ProblemKind::Incomplete);
            }
            _ => break,
        }
    }
    let root = and(nodes);
    Query {
        needs: root.needs(),
        root,
        directives: parser.directives,
        problems: parser.problems,
        mode,
    }
}

struct Parser {
    tokens: Vec<Spanned>,
    pos: usize,
    depth: usize,
    mode: Mode,
    problems: Vec<Problem>,
    directives: Directives,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|spanned| &spanned.token)
    }

    fn span(&self) -> Range<usize> {
        self.tokens
            .get(self.pos)
            .map_or(0..0, |spanned| spanned.span.clone())
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn problem(&mut self, span: Range<usize>, kind: ProblemKind) {
        self.problems.push(Problem { span, kind });
    }

    // A problem only rules have
    fn rule_problem(&mut self, span: Range<usize>, kind: ProblemKind) {
        if self.mode == Mode::Rule {
            self.problem(span, kind);
        }
    }

    // Terms with OR between them; `field` is the field of the parentheses
    // they're in, if any
    fn or(&mut self, field: Option<Field>) -> Node {
        let start = self.pos;
        let mut nodes = vec![self.and(field)];
        while self.peek() == Some(&Token::Or) {
            let span = self.span();
            // An OR at the start is ignored too
            if self.pos == start {
                self.rule_problem(span.clone(), ProblemKind::Incomplete);
            }
            self.pos += 1;
            // An OR at the end is ignored
            if matches!(self.peek(), None | Some(Token::Close | Token::Or)) {
                self.rule_problem(span, ProblemKind::Incomplete);
                continue;
            }
            nodes.push(self.and(field));
        }
        or(nodes)
    }

    fn and(&mut self, field: Option<Field>) -> Node {
        let mut nodes = Vec::new();
        loop {
            match self.peek() {
                None | Some(Token::Close | Token::Or) => break,
                Some(Token::LoneMinus) => {
                    let span = self.span();
                    self.pos += 1;
                    self.rule_problem(span, ProblemKind::Incomplete);
                }
                Some(Token::And) => {
                    let span = self.span();
                    self.pos += 1;
                    if nodes.is_empty()
                        || matches!(self.peek(), None | Some(Token::Close | Token::Or))
                    {
                        self.rule_problem(span, ProblemKind::Incomplete);
                    }
                }
                Some(_) => nodes.push(self.unary(field)),
            }
        }
        and(nodes)
    }

    fn unary(&mut self, field: Option<Field>) -> Node {
        // Each "-" or NOT turns the term around; they're counted rather than
        // nested, so that a long run of them can't go deep
        let mut negated = false;
        while matches!(self.peek(), Some(Token::Minus | Token::Not)) {
            let span = self.span();
            self.pos += 1;
            negated = !negated;
            if matches!(
                self.peek(),
                None | Some(Token::Close | Token::Or | Token::And)
            ) {
                self.rule_problem(span, ProblemKind::Incomplete);
                return Node::All;
            }
        }
        let node = match self.peek() {
            Some(Token::Open) => self.group(field),
            Some(Token::Word(_)) => self.term(field),
            _ => return Node::All,
        };
        // Leaving out an unfinished term leaves out nothing
        if negated && node != Node::All {
            not(node)
        } else {
            node
        }
    }

    // Parentheses; past the deepest they can go, they're ignored
    fn group(&mut self, field: Option<Field>) -> Node {
        let span = self.span();
        self.pos += 1;
        if self.depth >= MAX_DEPTH {
            if !self.problems.iter().any(|p| p.kind == ProblemKind::TooDeep) {
                self.problem(span, ProblemKind::TooDeep);
            }
            return Node::All;
        }
        self.depth += 1;
        let node = self.or(field);
        self.depth -= 1;
        match self.peek() {
            Some(Token::Close) => self.pos += 1,
            _ => self.rule_problem(span, ProblemKind::Incomplete),
        }
        node
    }

    fn term(&mut self, group_field: Option<Field>) -> Node {
        let span = self.span();
        let Some(Token::Word(word)) = self.peek().cloned() else {
            return Node::All;
        };
        self.pos += 1;
        let start = word.unquoted_start();

        // A field's name, before a colon, of letters only
        let name = start
            .find(':')
            .map(|colon| &start[..colon])
            .filter(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphabetic()));
        let Some(name) = name else {
            return match group_field {
                Some(field) => self.value_term(field, &word, span),
                None => self.word(&word, span),
            };
        };
        let value = word.after(name.len() + 1);

        if let Some(known) = Name::parse(name) {
            if group_field.is_some() {
                self.problem(span, ProblemKind::FieldInGroup);
                return Node::All;
            }
            return match known {
                Name::Field(field) => self.field_term(field, value, span),
                Name::Is => self.flag_term(true, &word, &value, span),
                Name::Has => self.flag_term(false, &word, &value, span),
                Name::Directive(directive) => {
                    self.directive(directive, &value, span);
                    Node::All
                }
            };
        }

        // Not a name the syntax knows, so it's text, as in "Re: Bach"
        match self.mode {
            Mode::Search => {
                if let Some(field) = near_name(name) {
                    self.problem(
                        span.clone(),
                        ProblemKind::DidYouMean {
                            typed: name.to_string(),
                            field,
                        },
                    );
                }
            }
            Mode::Rule => self.problem(span.clone(), ProblemKind::NotAField(name.to_string())),
        }
        match group_field {
            Some(field) => self.value_term(field, &word, span),
            None => self.word(&word, span),
        }
    }

    // A word or a phrase, searched in the text of the tags
    fn word(&mut self, word: &Word, span: Range<usize>) -> Node {
        let text = word.text();
        let start = word.unquoted_start();
        if start == "!" && word.pieces.len() == 1 {
            self.problem(span.clone(), ProblemKind::LoneBang);
        } else if start.starts_with(['<', '>']) {
            self.problem(span.clone(), ProblemKind::NoFieldToCompare);
        }
        self.check_reserved(word, span.clone());
        if word.unclosed {
            self.rule_problem(span, ProblemKind::Incomplete);
        }
        let folded = normalize_search_text(&text);
        if folded.is_empty() {
            Node::All
        } else {
            Node::Word(folded)
        }
    }

    // A field and what follows its colon
    fn field_term(&mut self, field: Field, value: Word, span: Range<usize>) -> Node {
        if value.unquoted_start().starts_with(':') {
            self.problem(span, ProblemKind::Reserved);
            return Node::All;
        }
        if !value.is_empty() {
            return self.value_term(field, &value, span);
        }
        // `composer:(bach OR handel)`, and Strawberry's spaces after the
        // colon, as in a pasted "Composer: Bach"
        match self.peek() {
            Some(Token::Open) => self.group(Some(field)),
            Some(Token::Word(next)) if !has_known_name(next) => {
                let span = self.span();
                let Some(Token::Word(value)) = self.peek().cloned() else {
                    unreachable!()
                };
                self.pos += 1;
                self.value_term(field, &value, span)
            }
            _ => {
                // Unfinished; at the end, it's being typed
                self.rule_problem(span, ProblemKind::Incomplete);
                Node::All
            }
        }
    }

    // A field's term, from its value with the operator before it
    fn value_term(&mut self, field: Field, value: &Word, span: Range<usize>) -> Node {
        let start = value.unquoted_start();
        let (op, op_len) = [
            ("==", Op::Equals),
            ("!=", Op::Equals),
            ("<>", Op::Equals),
            ("<=", Op::LessOrEqual),
            (">=", Op::GreaterOrEqual),
            ("=", Op::Equals),
            ("<", Op::Less),
            (">", Op::Greater),
        ]
        .into_iter()
        .find(|(prefix, _)| start.starts_with(prefix))
        .map_or((Op::Contains, 0), |(prefix, op)| (op, prefix.len()));
        let negated = start.starts_with("!=") || start.starts_with("<>");
        let value = value.after(op_len);
        self.check_reserved(&value, span.clone());
        if value.unclosed {
            self.rule_problem(span.clone(), ProblemKind::Incomplete);
        }
        let raw = value.text();
        if value.is_empty() {
            self.rule_problem(span, ProblemKind::Incomplete);
            return Node::All;
        }
        let at_end = self.at_end();

        let test = match field.kind() {
            Kind::Text => match op {
                Op::Contains => Ok(Test::Contains(normalize_search_text(&raw))),
                Op::Equals => {
                    let text = normalize_search_text(&raw);
                    if text.is_empty() {
                        self.rule_problem(span.clone(), ProblemKind::EqualsEmpty(field));
                    }
                    Ok(Test::Equals(text))
                }
                _ => Err(ProblemKind::CantCompare(field)),
            },
            Kind::Number => values::numbers(
                op,
                &raw,
                at_end,
                values::number,
                ProblemKind::NotANumber(field),
            ),
            Kind::Duration => values::durations(field, op, &raw, at_end),
            Kind::Date => values::dates(field, op, &raw, at_end),
            Kind::Catalogue => values::catalogue(op, &raw, at_end),
            Kind::Path => values::path(op, &raw),
        };
        let test = match test {
            Ok(test) => test,
            // A value still being typed is left out until it's whole
            Err(ProblemKind::Incomplete) => {
                self.rule_problem(span, ProblemKind::Incomplete);
                return Node::All;
            }
            Err(kind) => {
                self.problem(span, kind);
                return Node::All;
            }
        };
        let term = Node::Term(Term {
            field,
            op,
            raw,
            test,
        });
        if negated { not(term) } else { term }
    }

    // `is:` and `has:`, whose values follow their colons: a pasted "This
    // Is: Bach" is text
    fn flag_term(&mut self, is: bool, word: &Word, value: &Word, span: Range<usize>) -> Node {
        if value.is_empty() {
            if self.at_end() {
                self.rule_problem(span, ProblemKind::Incomplete);
                return Node::All;
            }
            return self.word(word, span);
        }
        let name = value.text();
        if is {
            if let Some(flag) = IsFlag::from_name(&name) {
                Node::Is(flag)
            } else {
                self.problem(span, ProblemKind::UnknownFlag(name));
                Node::All
            }
        } else {
            if let Some(field) = Field::from_name(&name) {
                Node::Has(field)
            } else {
                self.problem(span, ProblemKind::UnknownField(name));
                Node::All
            }
        }
    }

    // `sort:`, `pick:`, `limit:` and `by:`, which only rules take
    fn directive(&mut self, directive: Directive, value: &Word, span: Range<usize>) {
        if self.mode == Mode::Search {
            if !self
                .problems
                .iter()
                .any(|p| p.kind == ProblemKind::DirectiveInSearch)
            {
                self.problem(span, ProblemKind::DirectiveInSearch);
            }
            return;
        }
        let text = value.text();
        let bad = || ProblemKind::BadDirective(directive.name());
        let ok = match directive {
            Directive::Sort | Directive::Pick => {
                let keys: Option<Vec<SortKey>> = text
                    .split(',')
                    .map(|key| {
                        let (descending, name) = match key.strip_prefix('-') {
                            Some(name) => (true, name),
                            None => (false, key),
                        };
                        SortBy::from_name(name).map(|by| SortKey { by, descending })
                    })
                    .collect();
                match keys {
                    Some(keys) if !text.is_empty() => {
                        let list = if directive == Directive::Sort {
                            &mut self.directives.sort
                        } else {
                            &mut self.directives.pick
                        };
                        list.extend(keys);
                        true
                    }
                    _ => false,
                }
            }
            Directive::Limit => {
                let limit = match text.parse::<u32>() {
                    Ok(count) => Some(Limit::Count(count)),
                    Err(_) => values::duration(&text)
                        .and_then(|seconds| u64::try_from(seconds).ok())
                        .map(Limit::Duration),
                }
                .filter(|limit| !matches!(limit, Limit::Count(0) | Limit::Duration(0)));
                self.directives.limit = limit.or(self.directives.limit);
                limit.is_some()
            }
            Directive::By => {
                let unit = [Unit::Work, Unit::Album, Unit::Track]
                    .into_iter()
                    .find(|unit| unit.name().eq_ignore_ascii_case(&text));
                self.directives.by = unit.or(self.directives.by);
                unit.is_some()
            }
        };
        if !ok {
            self.problem(span, bad());
        }
    }

    // In a rule, values with the characters that wildcards will need must
    // be quoted
    fn check_reserved(&mut self, word: &Word, span: Range<usize>) {
        if self.mode != Mode::Rule {
            return;
        }
        let reserved = word
            .pieces
            .iter()
            .filter(|piece| !piece.quoted)
            .flat_map(|piece| piece.text.chars())
            .find(|c| RESERVED.contains(c));
        if let Some(character) = reserved {
            self.problem(span, ProblemKind::ReservedCharacter(character));
        }
    }
}

// Whether a word starts with the name of a field, a flag or a directive and
// its colon, so that a field before it doesn't take it as its value
fn has_known_name(word: &Word) -> bool {
    let start = word.unquoted_start();
    start
        .split_once(':')
        .is_some_and(|(name, _)| Name::parse(name).is_some())
}

// The terms together, without the ones that match every track
fn and(nodes: Vec<Node>) -> Node {
    let mut flat = Vec::new();
    for node in nodes {
        match node {
            Node::All => (),
            Node::And(inner) => flat.extend(inner),
            node => flat.push(node),
        }
    }
    match flat.len() {
        0 => Node::All,
        1 => flat.pop().unwrap(),
        _ => Node::And(flat),
    }
}

// Either of the terms. The ones that match every track are empty or
// unfinished, as in "bach OR composer:" while it's typed, so they're left
// out rather than making the whole match every track
fn or(nodes: Vec<Node>) -> Node {
    let mut flat = Vec::new();
    for node in nodes {
        match node {
            Node::All => (),
            Node::Or(inner) => flat.extend(inner),
            node => flat.push(node),
        }
    }
    match flat.len() {
        0 => Node::All,
        1 => flat.pop().unwrap(),
        _ => Node::Or(flat),
    }
}

fn not(node: Node) -> Node {
    match node {
        Node::Not(inner) => *inner,
        node => Node::Not(Box::new(node)),
    }
}

// The name of a field, a flag or a directive close to what was typed, as
// "composer" is to "composr", for a hint
fn near_name(typed: &str) -> Option<&'static str> {
    let typed = typed.to_ascii_lowercase();
    let len = typed.chars().count();
    if len < 4 {
        return None;
    }
    let most = if len >= 7 { 2 } else { 1 };
    Name::all()
        .map(|name| (distance(&typed, name), name))
        .filter(|(distance, _)| *distance <= most)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, name)| name)
}

// The edits from one text to another, a swap of two letters next to each
// other counting as one
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}
