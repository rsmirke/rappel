//! What `xgettext` would do, for the tests (ADR 0059): finds the marked
//! messages of the Rust sources and Blueprint files, with the comments for
//! translators, builds `po/rappel.pot` from them and merges a language's
//! `.po` with it, as `msgmerge` does. It also lists the strings that look
//! like text for the user but are not marked.
//!
//! A small lexer of each language, enough for this crate's sources: Rust
//! comments, string, raw string, byte string and character literals, and
//! lifetimes; Blueprint comments and strings. In Rust, a message is the
//! literal arguments of a call of `gettext`, `pgettext`, `ngettext` or
//! `npgettext` (the context first, then the singular and the plural); an
//! item under `#[cfg(test)]` is skipped. In Blueprint, `_("…")` and
//! `C_("context", "…")`. A comment starting `Translators:` goes with the
//! next message at most three lines below it.

use std::collections::HashMap;

use super::po::{self, Entry};

/// A marked message found in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub context: Option<String>,
    pub id: String,
    pub plural: Option<String>,
    /// The comment for translators, if any.
    pub comment: Option<String>,
    /// The file, relative to the repository.
    pub file: String,
    pub line: usize,
}

/// What a scan of one file found.
#[derive(Debug, Default)]
pub struct Scan {
    pub messages: Vec<Message>,
    /// Strings that look like text for the user and are not marked:
    /// (line, value).
    pub unmarked: Vec<(usize, String)>,
    /// Marked calls whose arguments are not literals, and the like.
    pub errors: Vec<String>,
}

/// Whether `text` looks like words for a person rather than an id, a key,
/// a CSS class or a path: two letters in a row (placeholders aside), and a
/// space, a capital letter followed by a small one, or a final "…".
pub fn is_prose(text: &str) -> bool {
    let mut bare = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        bare.push_str(&rest[..open]);
        if let Some(close) = rest[open..].find('}') {
            rest = &rest[open + close + 1..];
        } else {
            rest = &rest[open..];
            break;
        }
    }
    bare.push_str(rest);
    let chars: Vec<char> = bare.chars().collect();
    let word = chars
        .windows(2)
        .any(|pair| pair[0].is_alphabetic() && pair[1].is_alphabetic());
    let capital = chars
        .windows(2)
        .any(|pair| pair[0].is_uppercase() && pair[1].is_lowercase());
    // A path or a resource name ("/io/github/…") or a type name
    // ("RappelDeckRow") has no space.
    let camel = chars
        .windows(2)
        .any(|pair| pair[0].is_lowercase() && pair[1].is_uppercase());
    let path = !bare.contains(' ') && (bare.contains('/') || camel);
    // A key or an accelerator ("<Control>z", "KP_Enter", "Return").
    let key = !bare.contains(' ')
        && (bare.starts_with('<') || bare.contains("KP_") || KEY_NAMES.contains(&bare.as_str()));
    let path = path || key;
    word && !path && (bare.contains(' ') || capital || bare.ends_with('…'))
}

/// GDK key names that read like words.
const KEY_NAMES: [&str; 13] = [
    "Return",
    "Delete",
    "Escape",
    "BackSpace",
    "Tab",
    "Home",
    "End",
    "Up",
    "Down",
    "Left",
    "Right",
    "Menu",
    "Insert",
];

/// A token of either language, with its line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Str(String),
    /// A literal that is not text: a number, a character, a byte string.
    Other,
    Punct(char),
    Comment(String),
}

/// The Rust tokens of `src`.
#[expect(
    clippy::too_many_lines,
    reason = "one lexer loop, a branch per kind of token; split up it would be harder to follow"
)]
fn rust_tokens(src: &str) -> Result<Vec<(usize, Token)>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let at = |i: usize| chars.get(i).copied().unwrap_or('\0');
    while i < chars.len() {
        let c = chars[i];
        let start = line;
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == '/' {
            let end = chars[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |n| i + n);
            let text: String = chars[i + 2..end].iter().collect();
            tokens.push((start, Token::Comment(text)));
            i = end;
        } else if c == '/' && at(i + 1) == '*' {
            let mut depth = 0;
            loop {
                if i >= chars.len() {
                    return Err(format!("line {start}: unclosed comment"));
                }
                if chars[i] == '/' && at(i + 1) == '*' {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && at(i + 1) == '/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
            tokens.push((start, Token::Comment(String::new())));
        } else if c == '"' {
            let (value, end, lines) = rust_string(&chars, i + 1)
                .ok_or_else(|| format!("line {start}: unclosed string"))?;
            tokens.push((start, Token::Str(value)));
            line += lines;
            i = end;
        } else if c == '\'' {
            // A character literal, or a lifetime.
            if at(i + 1) == '\\' {
                let end = (i + 2..chars.len())
                    .find(|&j| chars[j] == '\'')
                    .ok_or_else(|| format!("line {start}: unclosed character"))?;
                tokens.push((start, Token::Other));
                i = end + 1;
            } else if at(i + 2) == '\'' {
                tokens.push((start, Token::Other));
                i += 3;
            } else {
                i += 1;
                while at(i).is_alphanumeric() || at(i) == '_' {
                    i += 1;
                }
            }
        } else if c.is_alphabetic() || c == '_' {
            let begin = i;
            while at(i).is_alphanumeric() || at(i) == '_' {
                i += 1;
            }
            let word: String = chars[begin..i].iter().collect();
            let raw = matches!(word.as_str(), "r" | "br" | "cr");
            let prefix = raw || matches!(word.as_str(), "b" | "c");
            if raw && (at(i) == '#' || at(i) == '"') {
                let hashes = chars[i..].iter().take_while(|&&c| c == '#').count();
                if at(i + hashes) == '"' {
                    // A raw string: up to a quote and as many #.
                    let body = i + hashes + 1;
                    let mut j = body;
                    let end = loop {
                        if j >= chars.len() {
                            return Err(format!("line {start}: unclosed raw string"));
                        }
                        if chars[j] == '"' && chars[j + 1..].iter().take(hashes).all(|&c| c == '#')
                        {
                            break j;
                        }
                        j += 1;
                    };
                    let value: String = chars[body..end].iter().collect();
                    line += value.matches('\n').count();
                    tokens.push((
                        start,
                        if word == "r" {
                            Token::Str(value)
                        } else {
                            Token::Other
                        },
                    ));
                    i = end + 1 + hashes;
                    continue;
                }
                // `r#ident`, a raw identifier.
                i += 1;
                let begin = i;
                while at(i).is_alphanumeric() || at(i) == '_' {
                    i += 1;
                }
                tokens.push((start, Token::Ident(chars[begin..i].iter().collect())));
                continue;
            }
            if prefix && at(i) == '"' {
                let (_, end, lines) = rust_string(&chars, i + 1)
                    .ok_or_else(|| format!("line {start}: unclosed string"))?;
                tokens.push((start, Token::Other));
                line += lines;
                i = end;
                continue;
            }
            if word == "b" && at(i) == '\'' {
                // A byte character: b'x' or b'\n'.
                let end = if at(i + 1) == '\\' {
                    (i + 3..chars.len())
                        .find(|&j| chars[j] == '\'')
                        .ok_or_else(|| format!("line {start}: unclosed byte"))?
                } else {
                    i + 2
                };
                tokens.push((start, Token::Other));
                i = end + 1;
                continue;
            }
            tokens.push((start, Token::Ident(word)));
        } else if c.is_ascii_digit() {
            while at(i).is_alphanumeric()
                || at(i) == '_'
                || at(i) == '.' && at(i + 1).is_ascii_digit()
            {
                i += 1;
            }
            tokens.push((start, Token::Other));
        } else {
            tokens.push((start, Token::Punct(c)));
            i += 1;
        }
    }
    Ok(tokens)
}

/// The value of a Rust string whose body starts at `from`: (value, the
/// index after its closing quote, the line breaks it holds).
fn rust_string(chars: &[char], from: usize) -> Option<(String, usize, usize)> {
    let mut value = String::new();
    let mut lines = 0;
    let mut i = from;
    loop {
        let c = *chars.get(i)?;
        match c {
            '"' => return Some((value, i + 1, lines)),
            '\\' => {
                let next = *chars.get(i + 1)?;
                i += 2;
                match next {
                    'n' => value.push('\n'),
                    't' => value.push('\t'),
                    'r' => value.push('\r'),
                    '0' => value.push('\0'),
                    '\\' => value.push('\\'),
                    '"' => value.push('"'),
                    '\'' => value.push('\''),
                    'x' => {
                        let hex: String = chars.get(i..i + 2)?.iter().collect();
                        value.push(char::from(u8::from_str_radix(&hex, 16).ok()?));
                        i += 2;
                    }
                    'u' => {
                        let close = i + chars[i..].iter().position(|&c| c == '}')?;
                        let hex: String = chars[i + 1..close].iter().collect();
                        value.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                        i = close + 1;
                    }
                    '\n' => {
                        // A line continuation: the break and the next
                        // line's leading whitespace are not in the string.
                        lines += 1;
                        while chars.get(i).is_some_and(|c| c.is_whitespace()) {
                            if chars[i] == '\n' {
                                lines += 1;
                            }
                            i += 1;
                        }
                    }
                    _ => return None,
                }
            }
            c => {
                if c == '\n' {
                    lines += 1;
                }
                value.push(c);
                i += 1;
            }
        }
    }
}

/// The marking functions of the Rust sources and the literal arguments each
/// takes, in order: (name, has a context, has a plural).
const RUST_KEYWORDS: [(&str, bool, bool); 4] = [
    ("gettext", false, false),
    ("pgettext", true, false),
    ("ngettext", false, true),
    ("npgettext", true, true),
];

/// Calls and macros whose strings are not for the user: logs, panics,
/// assertions and the like.
const NOT_FOR_THE_USER: [&str; 21] = [
    "write_str",
    "debug_struct",
    "debug_tuple",
    "field",
    "trace",
    "debug",
    "info",
    "warn",
    "error",
    "panic",
    "expect",
    "unreachable",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "compile_error",
    "include_str",
    "include_bytes",
    "resources_register_include",
];

/// Arguments that only go to the log: the "what" of `ui::toast::show_error`,
/// `log_failure`, `ErrorSlot::show` and `show_in_preferences` ("an export
/// failed" is logged; the message after it is shown). (callee, argument
/// index from 0).
/// A method is named with a leading "." (`errors.show(widget, what, text)`).
const LOGGED_ARGUMENTS: [(&str, usize); 6] = [
    // `preferences::saved(ctx, dialog, what, result)`.
    ("saved", 2),
    ("show_error", 1),
    ("log_failure", 0),
    (".show", 1),
    (".show_in_preferences", 1),
    // `clean_up::Context::show_error(what, text)`.
    (".show_error", 0),
];

/// An open bracket of the Rust parse.
#[derive(Debug, Default)]
struct Frame {
    /// The function or macro called with this bracket, if any.
    callee: Option<String>,
    /// Which argument of the bracket the tokens are in (commas so far).
    arg: usize,
    /// `#[…]`.
    attribute: bool,
    /// A marking call: its keyword, line and comment, and its arguments
    /// so far (`Some(literal)`, or `None` for anything else).
    call: Option<Call>,
    /// The tokens inside an attribute, to recognise `cfg(test)`.
    words: Vec<String>,
}

#[derive(Debug)]
struct Call {
    keyword: usize,
    line: usize,
    comment: Option<String>,
    args: Vec<Option<String>>,
    /// Whether the current argument has had a token yet.
    started: bool,
}

/// The comment for translators waiting for its message.
#[derive(Debug)]
struct Pending {
    text: String,
    last_line: usize,
}

/// How many lines a comment for translators may be above its message.
const COMMENT_REACH: usize = 3;

/// Scans a Rust source file.
#[expect(
    clippy::too_many_lines,
    reason = "one pass over the tokens with its bracket stack; split up it would be harder to follow"
)]
pub fn scan_rust(file: &str, src: &str) -> Scan {
    let mut scan = Scan::default();
    let tokens = match rust_tokens(src) {
        Ok(tokens) => tokens,
        Err(err) => {
            scan.errors.push(format!("{file}: {err}"));
            return scan;
        }
    };
    let mut stack: Vec<Frame> = Vec::new();
    let mut pending: Option<Pending> = None;
    let mut previous: Vec<&Token> = Vec::new();
    // Set after `#[cfg(test)]`: the next item is skipped.
    let mut skip_item = false;
    let mut i = 0;
    while i < tokens.len() {
        let (line, token) = (&tokens[i].0, &tokens[i].1);
        let line = *line;
        if let Token::Comment(text) = token {
            let text = text.trim_start_matches(['/', '!']).trim();
            match &mut pending {
                Some(waiting) if waiting.last_line + 1 == line && !text.is_empty() => {
                    waiting.text.push(' ');
                    waiting.text.push_str(text);
                    waiting.last_line = line;
                }
                _ if text.starts_with("Translators:") => {
                    pending = Some(Pending {
                        text: text.to_owned(),
                        last_line: line,
                    });
                }
                _ => {}
            }
            i += 1;
            continue;
        }
        if skip_item && !matches!(token, Token::Punct('#')) {
            skip_item = false;
            i = item_end(&tokens, i);
            previous.clear();
            continue;
        }
        if let Token::Ident(word) = token {
            for frame in stack.iter_mut().filter(|frame| frame.attribute) {
                frame.words.push(word.clone());
            }
        }
        if let Some(frame) = stack.last_mut() {
            if *token == Token::Punct(',') {
                frame.arg += 1;
            }
            if let Some(call) = &mut frame.call {
                match token {
                    Token::Punct(',') => {
                        call.started = false;
                    }
                    Token::Punct(')') => {}
                    Token::Str(value) if !call.started => {
                        call.args.push(Some(value.clone()));
                        call.started = true;
                    }
                    _ => {
                        if call.started {
                            if let Some(last) = call.args.last_mut() {
                                *last = None;
                            }
                        } else {
                            call.args.push(None);
                            call.started = true;
                        }
                    }
                }
            }
        }
        match token {
            Token::Punct(open @ ('(' | '[' | '{')) => {
                let mut frame = Frame::default();
                match previous.as_slice() {
                    [.., Token::Punct('.'), Token::Ident(name)] => {
                        frame.callee = Some(format!(".{name}"));
                    }
                    [.., Token::Ident(name)] => frame.callee = Some(name.clone()),
                    [.., Token::Ident(name), Token::Punct('!')] => {
                        frame.callee = Some(name.clone());
                    }
                    _ => {}
                }
                frame.attribute = *open == '['
                    && matches!(
                        previous.as_slice(),
                        [.., Token::Punct('#')] | [.., Token::Punct('#'), Token::Punct('!')]
                    );
                let defined = matches!(
                    previous.as_slice(),
                    [.., Token::Ident(word), Token::Ident(_)] if word == "fn"
                );
                let method = matches!(
                    previous.as_slice(),
                    [.., Token::Punct('.'), Token::Ident(_)]
                );
                let keyword = frame
                    .callee
                    .as_deref()
                    .and_then(|name| RUST_KEYWORDS.iter().position(|(word, _, _)| *word == name));
                if *open == '('
                    && !defined
                    && !method
                    && let Some(keyword) = keyword
                {
                    let comment = pending
                        .take()
                        .filter(|waiting| line - waiting.last_line <= COMMENT_REACH)
                        .map(|waiting| waiting.text);
                    frame.call = Some(Call {
                        keyword,
                        line,
                        comment,
                        args: Vec::new(),
                        started: false,
                    });
                }
                stack.push(frame);
            }
            Token::Punct(')' | ']' | '}') => {
                if let Some(frame) = stack.pop() {
                    if frame.attribute && frame.words == ["cfg", "test"] {
                        skip_item = true;
                    }
                    if let Some(call) = frame.call {
                        finish_call(file, call, &mut scan);
                    }
                    // The closed bracket is an argument token of an
                    // enclosing call: handled when it was opened.
                }
            }
            Token::Str(value) => {
                let marked = stack.iter().any(|frame| frame.call.is_some());
                let ignored = stack.iter().any(|frame| {
                    let callee = frame.callee.as_deref().unwrap_or("");
                    frame.attribute
                        || NOT_FOR_THE_USER.contains(&callee.trim_start_matches('.'))
                        || LOGGED_ARGUMENTS.contains(&(callee, frame.arg))
                });
                if !marked && !ignored && is_prose(value) {
                    scan.unmarked.push((line, value.clone()));
                }
            }
            _ => {}
        }
        if let Some(waiting) = &pending
            && line > waiting.last_line + COMMENT_REACH
        {
            pending = None;
        }
        previous.push(token);
        if previous.len() > 3 {
            previous.remove(0);
        }
        i += 1;
    }
    scan
}

/// The index after the item starting at `from`: its `;` at the top level,
/// or the end of its first top-level `{…}` block.
fn item_end(tokens: &[(usize, Token)], from: usize) -> usize {
    let mut depth = 0_usize;
    let mut i = from;
    while i < tokens.len() {
        match tokens[i].1 {
            Token::Punct('(' | '[' | '{') => depth += 1,
            Token::Punct(')' | ']') => depth = depth.saturating_sub(1),
            Token::Punct('}') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            Token::Punct(';') if depth == 0 => return i + 1,
            _ => {}
        }
        i += 1;
    }
    i
}

fn finish_call(file: &str, call: Call, scan: &mut Scan) {
    let (name, has_context, has_plural) = RUST_KEYWORDS[call.keyword];
    let wanted = 1 + usize::from(has_context) + usize::from(has_plural);
    let literals: Option<Vec<String>> = call.args.iter().take(wanted).cloned().collect();
    match literals {
        Some(literals) if literals.len() == wanted && literals.iter().all(|s| !s.is_empty()) => {
            let mut values = literals.into_iter();
            let context = if has_context { values.next() } else { None };
            let id = values.next().unwrap_or_default();
            let plural = if has_plural { values.next() } else { None };
            scan.messages.push(Message {
                context,
                id,
                plural,
                comment: call.comment,
                file: file.to_owned(),
                line: call.line,
            });
        }
        _ => scan.errors.push(format!(
            "{file}:{}: {name}() needs {wanted} non-empty string literals first",
            call.line
        )),
    }
}

/// The Blueprint tokens of `src`. Identifiers may hold `-`
/// (`tooltip-text`).
fn blueprint_tokens(src: &str) -> Result<Vec<(usize, Token)>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut line = 1;
    let at = |i: usize| chars.get(i).copied().unwrap_or('\0');
    while i < chars.len() {
        let c = chars[i];
        let start = line;
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == '/' {
            let end = chars[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |n| i + n);
            tokens.push((start, Token::Comment(chars[i + 2..end].iter().collect())));
            i = end;
        } else if c == '/' && at(i + 1) == '*' {
            let end = (i + 2..chars.len())
                .find(|&j| chars[j] == '*' && at(j + 1) == '/')
                .ok_or_else(|| format!("line {start}: unclosed comment"))?;
            let text: String = chars[i + 2..end].iter().collect();
            line += text.matches('\n').count();
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            // A comment's line is its last one: a message below it is
            // reached from there.
            tokens.push((line, Token::Comment(text)));
            i = end + 2;
        } else if c == '"' || c == '\'' {
            let mut value = String::new();
            let mut j = i + 1;
            loop {
                match chars.get(j) {
                    None => return Err(format!("line {start}: unclosed string")),
                    Some(&q) if q == c => break,
                    Some('\\') => {
                        match at(j + 1) {
                            'n' => value.push('\n'),
                            't' => value.push('\t'),
                            other => value.push(other),
                        }
                        j += 2;
                    }
                    Some(&other) => {
                        if other == '\n' {
                            line += 1;
                        }
                        value.push(other);
                        j += 1;
                    }
                }
            }
            tokens.push((start, Token::Str(value)));
            i = j + 1;
        } else if c.is_alphanumeric() || c == '_' || c == '$' {
            let begin = i;
            while at(i).is_alphanumeric() || at(i) == '_' || at(i) == '-' || at(i) == '$' {
                i += 1;
            }
            tokens.push((start, Token::Ident(chars[begin..i].iter().collect())));
        } else {
            tokens.push((start, Token::Punct(c)));
            i += 1;
        }
    }
    Ok(tokens)
}

/// Blueprint properties whose value is never shown: ids, names of
/// actions, icons, responses and pages, conditions, keys.
const BLUEPRINT_NOT_SHOWN: [&str; 11] = [
    "icon-name",
    "name",
    "action-name",
    "action",
    "close-response",
    "default-response",
    "active-name",
    "accelerator",
    "condition",
    "styles",
    "accel",
];

/// Scans a Blueprint file.
pub fn scan_blueprint(file: &str, src: &str) -> Scan {
    let mut scan = Scan::default();
    let tokens = match blueprint_tokens(src) {
        Ok(tokens) => tokens,
        Err(err) => {
            scan.errors.push(format!("{file}: {err}"));
            return scan;
        }
    };
    let mut pending: Option<Pending> = None;
    // The property or list a string is the value of: the last name before
    // a `:`, or the name before an open bracket.
    let mut owners: Vec<Option<String>> = Vec::new();
    let mut property: Option<String> = None;
    let mut i = 0;
    while i < tokens.len() {
        let (line, token) = (tokens[i].0, &tokens[i].1);
        let next = |n: usize| tokens.get(i + n).map(|(_, token)| token);
        match token {
            Token::Comment(text) => {
                let text = text.trim();
                if text.starts_with("Translators:") {
                    pending = Some(Pending {
                        text: text.to_owned(),
                        last_line: line,
                    });
                }
            }
            Token::Ident(name)
                if (name == "_" || name == "C_") && next(1) == Some(&Token::Punct('(')) =>
            {
                let comment = pending
                    .take()
                    .filter(|waiting| line - waiting.last_line <= COMMENT_REACH)
                    .map(|waiting| waiting.text);
                let found = match (name.as_str(), next(2), next(3), next(4), next(5)) {
                    ("_", Some(Token::Str(id)), Some(Token::Punct(')')), _, _) => {
                        Some((None, id.clone(), 4))
                    }
                    (
                        "C_",
                        Some(Token::Str(context)),
                        Some(Token::Punct(',')),
                        Some(Token::Str(id)),
                        Some(Token::Punct(')')),
                    ) => Some((Some(context.clone()), id.clone(), 6)),
                    _ => None,
                };
                match found {
                    Some((context, id, length)) if !id.is_empty() => {
                        scan.messages.push(Message {
                            context,
                            id,
                            plural: None,
                            comment,
                            file: file.to_owned(),
                            line,
                        });
                        i += length;
                        continue;
                    }
                    _ => scan
                        .errors
                        .push(format!("{file}:{line}: {name}() needs string literals")),
                }
            }
            Token::Ident(name) => {
                if next(1) == Some(&Token::Punct(':')) {
                    property = Some(name.clone());
                }
            }
            Token::Punct('(' | '[' | '{') => {
                let owner = match i.checked_sub(1).map(|j| &tokens[j].1) {
                    Some(Token::Ident(name)) => Some(name.clone()),
                    _ => property.clone(),
                };
                owners.push(owner);
                property = None;
            }
            Token::Punct(')' | ']' | '}') => {
                owners.pop();
                property = None;
            }
            Token::Punct(';') => property = None,
            Token::Str(value) => {
                let owner = property
                    .as_deref()
                    .or_else(|| owners.iter().rev().find_map(Option::as_deref));
                let hidden = owner.is_some_and(|name| BLUEPRINT_NOT_SHOWN.contains(&name))
                    || owners
                        .iter()
                        .flatten()
                        .any(|name| name == "styles" || name == "condition");
                if !hidden && is_prose(value) {
                    scan.unmarked.push((line, value.clone()));
                }
            }
            _ => {}
        }
        i += 1;
    }
    scan
}

/// The header of `po/rappel.pot`. No creation date, so the file changes
/// only when a message does.
fn template_header() -> Entry {
    Entry {
        comments: vec![
            "Rappel's messages, for translators.".to_owned(),
            "This file is distributed under the same licence as Rappel (GPL-3.0-or-later)."
                .to_owned(),
            "Made by the tests from the sources; do not edit it by hand (README.md).".to_owned(),
        ],
        strs: vec![
            "Project-Id-Version: rappel\n\
             Report-Msgid-Bugs-To: https://github.com/rsmirke/rappel/issues\n\
             MIME-Version: 1.0\n\
             Content-Type: text/plain; charset=UTF-8\n\
             Content-Transfer-Encoding: 8bit\n\
             Plural-Forms: nplurals=INTEGER; plural=EXPRESSION;\n"
                .to_owned(),
        ],
        ..Entry::default()
    }
}

/// The template's entries: the header, then each message once, in the
/// order first found, with every file it is in and every comment. Errors
/// for a message whose plural differs between two uses.
pub fn template(messages: &[Message]) -> Result<Vec<Entry>, Vec<String>> {
    let mut entries = vec![template_header()];
    let mut index: HashMap<(Option<String>, String), usize> = HashMap::new();
    let mut errors = Vec::new();
    for message in messages {
        let key = (message.context.clone(), message.id.clone());
        let at = *index.entry(key).or_insert_with(|| {
            entries.push(Entry {
                context: message.context.clone(),
                id: message.id.clone(),
                plural: message.plural.clone(),
                strs: vec![String::new(); if message.plural.is_some() { 2 } else { 1 }],
                ..Entry::default()
            });
            entries.len() - 1
        });
        let entry = &mut entries[at];
        if entry.plural != message.plural {
            errors.push(format!(
                "{}:{}: {:?} is used with two different plurals",
                message.file, message.line, message.id
            ));
        }
        if !entry.references.contains(&message.file) {
            entry.references.push(message.file.clone());
        }
        if let Some(comment) = &message.comment
            && !entry.extracted.contains(comment)
        {
            entry.extracted.push(comment.clone());
        }
    }
    if errors.is_empty() {
        Ok(entries)
    } else {
        Err(errors)
    }
}

/// A language's catalog brought up to date with the template, as
/// `msgmerge` does: its own header, the template's messages in the
/// template's order with their translations, flags and translators'
/// comments kept, new ones untranslated (`forms` empty strings for a
/// plural); messages no longer in the template are returned apart, to be
/// kept as obsolete (`#~`) so no translation is lost.
pub fn merge(template: &[Entry], old: &[Entry], forms: usize) -> (Vec<Entry>, Vec<Entry>) {
    let mut by_key: HashMap<String, &Entry> =
        old.iter().map(|entry| (entry.key(), entry)).collect();
    let mut merged = Vec::with_capacity(template.len());
    if let Some(header) = by_key.remove("") {
        merged.push(header.clone());
    }
    for wanted in template.iter().filter(|entry| !entry.id.is_empty()) {
        let mut entry = wanted.clone();
        match by_key.remove(&wanted.key()) {
            Some(had) if had.plural.is_some() == wanted.plural.is_some() => {
                entry.strs.clone_from(&had.strs);
                entry.flags.clone_from(&had.flags);
                entry.comments.clone_from(&had.comments);
                if had.plural != wanted.plural {
                    entry.flags.push("fuzzy".to_owned());
                }
            }
            _ => {
                let n = if wanted.plural.is_some() { forms } else { 1 };
                entry.strs = vec![String::new(); n];
            }
        }
        merged.push(entry);
    }
    let mut obsolete: Vec<Entry> = old
        .iter()
        .filter(|entry| by_key.contains_key(&entry.key()))
        .cloned()
        .collect();
    obsolete.retain(Entry::is_translated);
    (merged, obsolete)
}

/// Obsolete entries as `#~` lines, which every reader skips.
pub fn obsolete_text(entries: &[Entry]) -> String {
    let mut out = String::new();
    for entry in entries {
        let bare = Entry {
            comments: Vec::new(),
            extracted: Vec::new(),
            references: Vec::new(),
            flags: Vec::new(),
            ..entry.clone()
        };
        out.push('\n');
        for line in po::write(&[bare]).lines() {
            out.push_str("#~ ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}
