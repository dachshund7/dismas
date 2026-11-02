// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! Splits the text of a search into words, parentheses and operators. Only
//! ASCII `-`, `"`, `(` and `)` are syntax, and `AND`, `OR` and `NOT` in
//! capitals

use std::{iter::Peekable, ops::Range, str::CharIndices};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Token {
    Open,
    Close,
    // Before a term, which it leaves out
    Minus,
    // A "-" with nothing after it, which a search ignores
    LoneMinus,
    And,
    Or,
    Not,
    Word(Word),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Spanned {
    pub(crate) token: Token,
    // In bytes of the text
    pub(crate) span: Range<usize>,
}

// A run of text up to a space or a parenthesis, in which quotes can keep
// spaces and parentheses
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Word {
    pub(crate) pieces: Vec<Piece>,
    // Whether its last quote isn't closed, as while it's being typed
    pub(crate) unclosed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Piece {
    pub(crate) text: String,
    pub(crate) quoted: bool,
}

impl Word {
    // The text, without the quotes
    pub(crate) fn text(&self) -> String {
        self.pieces
            .iter()
            .map(|piece| piece.text.as_str())
            .collect()
    }

    // The text after the first `at` bytes of its first piece, which must be
    // unquoted, keeping the pieces after it
    pub(crate) fn after(&self, at: usize) -> Word {
        let mut pieces = Vec::new();
        if let Some(first) = self.pieces.first() {
            let rest = &first.text[at.min(first.text.len())..];
            if !rest.is_empty() {
                pieces.push(Piece {
                    text: rest.to_string(),
                    quoted: first.quoted,
                });
            }
        }
        pieces.extend(self.pieces.iter().skip(1).cloned());
        Word {
            pieces,
            unclosed: self.unclosed,
        }
    }

    // The text the unquoted start of the word begins with
    pub(crate) fn unquoted_start(&self) -> &str {
        match self.pieces.first() {
            Some(piece) if !piece.quoted => &piece.text,
            _ => "",
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }
}

pub(super) fn tokens(text: &str) -> Vec<Spanned> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    // Whether a "-" here would start a term
    let mut term_start = true;
    while let Some(&(start, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            term_start = true;
            continue;
        }
        match c {
            '(' => {
                chars.next();
                tokens.push(Spanned {
                    token: Token::Open,
                    span: start..start + 1,
                });
                term_start = true;
            }
            ')' => {
                chars.next();
                tokens.push(Spanned {
                    token: Token::Close,
                    span: start..start + 1,
                });
                term_start = false;
            }
            '-' if term_start => {
                chars.next();
                let lone = chars
                    .peek()
                    .is_none_or(|(_, next)| next.is_whitespace() || *next == ')');
                tokens.push(Spanned {
                    token: if lone { Token::LoneMinus } else { Token::Minus },
                    span: start..start + 1,
                });
            }
            _ => {
                let (word, end) = read_word(start, &mut chars);
                let token = match (word.pieces.as_slice(), word.unclosed) {
                    (
                        [
                            Piece {
                                text,
                                quoted: false,
                            },
                        ],
                        false,
                    ) => keyword(text).unwrap_or(Token::Word(word)),
                    _ => Token::Word(word),
                };
                tokens.push(Spanned {
                    token,
                    span: start..end,
                });
                term_start = false;
            }
        }
    }
    tokens
}

// The operator a word is, unquoted; only in capitals, so that "and" and
// "or" are searched for as words
pub(super) fn keyword(text: &str) -> Option<Token> {
    match text {
        "AND" => Some(Token::And),
        "OR" => Some(Token::Or),
        "NOT" => Some(Token::Not),
        _ => None,
    }
}

// Reads a word from its first character, and returns it and where it ends.
// In quotes, `\"` and `\\` are a quote and a backslash
fn read_word(start: usize, chars: &mut Peekable<CharIndices>) -> (Word, usize) {
    let mut word = Word::default();
    let mut unquoted = String::new();
    let mut end = start;
    while let Some(&(i, c)) = chars.peek() {
        if c.is_whitespace() || c == '(' || c == ')' {
            break;
        }
        chars.next();
        end = i + c.len_utf8();
        if c != '"' {
            unquoted.push(c);
            continue;
        }

        if !unquoted.is_empty() {
            word.pieces.push(Piece {
                text: std::mem::take(&mut unquoted),
                quoted: false,
            });
        }
        let mut quoted = String::new();
        let mut closed = false;
        while let Some((j, c)) = chars.next() {
            end = j + c.len_utf8();
            match c {
                '"' => {
                    closed = true;
                    break;
                }
                '\\' => match chars.peek() {
                    Some(&(k, escaped @ ('"' | '\\'))) => {
                        chars.next();
                        end = k + 1;
                        quoted.push(escaped);
                    }
                    _ => quoted.push('\\'),
                },
                _ => quoted.push(c),
            }
        }
        word.pieces.push(Piece {
            text: quoted,
            quoted: true,
        });
        if !closed {
            word.unclosed = true;
            return (word, end);
        }
    }
    if !unquoted.is_empty() {
        word.pieces.push(Piece {
            text: unquoted,
            quoted: false,
        });
    }
    (word, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<String> {
        tokens(text)
            .into_iter()
            .map(|spanned| match spanned.token {
                Token::Open => "(".to_string(),
                Token::Close => ")".to_string(),
                Token::Minus => "-".to_string(),
                Token::LoneMinus => "lone -".to_string(),
                Token::And => "AND".to_string(),
                Token::Or => "OR".to_string(),
                Token::Not => "NOT".to_string(),
                Token::Word(word) => format!("[{}]", word.text()),
            })
            .collect()
    }

    #[test]
    fn words_parentheses_and_operators() {
        assert_eq!(
            kinds("(bach OR handel) -live NOT x and"),
            [
                "(", "[bach]", "OR", "[handel]", ")", "-", "[live]", "NOT", "[x]", "[and]"
            ]
        );
        // A "-" inside a word, or after a colon, isn't one
        assert_eq!(
            kinds("blink-182 played:-7d"),
            ["[blink-182]", "[played:-7d]"]
        );
        // Nor one with nothing after it, nor a minus sign
        assert_eq!(
            kinds("a - b −c -"),
            ["[a]", "lone -", "[b]", "[−c]", "lone -"]
        );
    }

    #[test]
    fn quotes_keep_spaces_and_parentheses() {
        let tokens = tokens(r#"title:"ach (gott)" "a \"b\" \\""#);
        let Token::Word(word) = &tokens[0].token else {
            panic!()
        };
        assert_eq!(word.pieces[0].text, "title:");
        assert!(!word.pieces[0].quoted);
        assert_eq!(word.pieces[1].text, "ach (gott)");
        assert!(word.pieces[1].quoted);
        let Token::Word(word) = &tokens[1].token else {
            panic!()
        };
        assert_eq!(word.text(), r#"a "b" \"#);
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn an_unclosed_quote_runs_to_the_end() {
        let tokens = tokens(r#"x "ach gott"#);
        let Token::Word(word) = &tokens[1].token else {
            panic!()
        };
        assert!(word.unclosed);
        assert_eq!(word.text(), "ach gott");
        assert_eq!(tokens[1].span, 2..11);
    }
}
