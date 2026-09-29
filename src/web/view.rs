//! Views: a small subset of Laravel's Blade, rendered at request time from the
//! HTML Vite built into `public/build/views/`.
//!
//! - `{{ post.title }}` prints escaped; `{!! post.body !!}` prints as-is.
//! - `@if(comments)` / `@if(!comments)`, `@else`, `@endif`: true when the
//!   value is `true`, non-empty text or a non-empty list or map.
//! - `@foreach(posts as post)` … `@endforeach`.
//! - `@extends('layouts.app')` with `@section('content')` … `@endsection`,
//!   or `@section('title', post.title)`, shown by the layout's `@yield('title')`.
//! - `@include('partials.nav')` renders another view with the same data;
//!   `@include('components.card', post: featured, compact: true)` adds named
//!   values (a name, `'text'`, `true` or `false`) for that view only.
//! - `{{-- comment --}}` is dropped; `@@` prints `@` and `@{{` prints `{{`.
//!
//! View names use `.` or `/` between folders. There are no expressions: the
//! controller computes what the view shows. An unknown name is an error, a
//! missing key of a map is empty text.

use std::collections::HashMap;

use super::escape;

/// Data a view reads: text, a flag, a list or a map of named values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Printed escaped by `{{ }}`.
    Text(String),
    /// `true` prints `1`, `false` prints nothing.
    Bool(bool),
    /// Walked by `@foreach`.
    List(Vec<Value>),
    /// Read by `name.key`.
    Map(Vec<(String, Value)>),
}

impl Value {
    /// A map from `(name, value)` pairs, the shape [`render`](super::render)
    /// takes. Implement [`ToValue`] for a model with it:
    ///
    /// ```
    /// use rustclamp::web::{ToValue, Value};
    ///
    /// struct Post { slug: String, title: String }
    ///
    /// impl ToValue for Post {
    ///     fn to_value(&self) -> Value {
    ///         Value::map(&[("slug", &self.slug), ("title", &self.title)])
    ///     }
    /// }
    /// ```
    pub fn map(pairs: &[(&str, &dyn ToValue)]) -> Self {
        Self::Map(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.to_value()))
                .collect(),
        )
    }

    fn truthy(&self) -> bool {
        match self {
            Self::Text(text) => !text.is_empty(),
            Self::Bool(flag) => *flag,
            Self::List(items) => !items.is_empty(),
            Self::Map(pairs) => !pairs.is_empty(),
        }
    }
}

/// Anything a view can show.
pub trait ToValue {
    /// This value as view data.
    fn to_value(&self) -> Value;
}

impl ToValue for Value {
    fn to_value(&self) -> Value {
        self.clone()
    }
}

impl ToValue for str {
    fn to_value(&self) -> Value {
        Value::Text(self.to_owned())
    }
}

impl ToValue for String {
    fn to_value(&self) -> Value {
        Value::Text(self.clone())
    }
}

impl ToValue for bool {
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }
}

macro_rules! numbers {
    ($($number:ty),*) => {$(
        impl ToValue for $number {
            fn to_value(&self) -> Value {
                Value::Text(self.to_string())
            }
        }
    )*};
}
numbers!(i32, i64, u16, u32, u64, usize, f64);

impl<T: ToValue> ToValue for [T] {
    fn to_value(&self) -> Value {
        Value::List(self.iter().map(ToValue::to_value).collect())
    }
}

impl<T: ToValue> ToValue for Vec<T> {
    fn to_value(&self) -> Value {
        self.as_slice().to_value()
    }
}

/// `None` is `false`, so `@if(user)` skips it.
impl<T: ToValue> ToValue for Option<T> {
    fn to_value(&self) -> Value {
        self.as_ref().map_or(Value::Bool(false), ToValue::to_value)
    }
}

impl<T: ToValue + ?Sized> ToValue for &T {
    fn to_value(&self) -> Value {
        (**self).to_value()
    }
}

/// How deep `@include` and `@extends` may nest, so a view including itself
/// fails instead of overflowing the stack.
const MAX_DEPTH: usize = 32;

/// Renders `source`, the view `name`, with `data` (a [`Value::Map`]). `load`
/// reads the views it extends or includes by name. Errors name the view and line.
pub(super) fn render(
    name: &str,
    source: &str,
    data: &Value,
    load: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let mut scope = match data {
        Value::Map(pairs) => pairs.clone(),
        _ => Vec::new(),
    };
    let mut renderer = Renderer {
        load,
        sections: HashMap::new(),
        depth: 0,
    };
    renderer.view(name, source, &mut scope)
}

#[derive(Debug)]
enum Node {
    Text(String),
    Echo {
        path: String,
        raw: bool,
        line: usize,
    },
    If {
        path: String,
        not: bool,
        then: Vec<Node>,
        otherwise: Vec<Node>,
        line: usize,
    },
    Foreach {
        list: String,
        item: String,
        body: Vec<Node>,
        line: usize,
    },
    Section {
        name: String,
        body: Vec<Node>,
    },
    Yield(String),
    Include {
        name: String,
        with: Vec<(String, Arg)>,
        line: usize,
    },
}

/// A value passed to `@include`: a name read from the caller's data, or a literal.
#[derive(Debug)]
enum Arg {
    Path(String),
    Literal(Value),
}

/// A parsed view: the layout it extends, if any, and its nodes.
struct Template {
    extends: Option<String>,
    nodes: Vec<Node>,
}

struct Renderer<'a> {
    load: &'a dyn Fn(&str) -> Option<String>,
    /// Section bodies, rendered; the first definition (the child's) wins.
    sections: HashMap<String, String>,
    depth: usize,
}

impl Renderer<'_> {
    // ponytail: parses the view and its layout on every request; cache parsed
    // templates if profiling shows it matters.
    fn view(
        &mut self,
        name: &str,
        source: &str,
        scope: &mut Vec<(String, Value)>,
    ) -> Result<String, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(format!(
                "view {name}: nested more than {MAX_DEPTH} views deep"
            ));
        }
        let template = parse(source).map_err(|problem| format!("view {name} {problem}"))?;
        let mut out = String::new();
        self.nodes(&template.nodes, scope, &mut out)
            .map_err(|problem| format!("view {name} {problem}"))?;
        let out = match template.extends {
            // What the child printed outside its sections is dropped, as in Blade.
            Some(layout) => {
                let source = (self.load)(&layout)
                    .ok_or_else(|| format!("view {name}: layout {layout} not found"))?;
                self.view(&layout, &source, scope)?
            }
            None => out,
        };
        self.depth -= 1;
        Ok(out)
    }

    fn nodes(
        &mut self,
        nodes: &[Node],
        scope: &mut Vec<(String, Value)>,
        out: &mut String,
    ) -> Result<(), String> {
        for node in nodes {
            match node {
                Node::Text(text) => out.push_str(text),
                Node::Echo { path, raw, line } => {
                    match lookup(scope, path).map_err(|problem| at(*line, &problem))? {
                        Value::Text(text) if *raw => out.push_str(&text),
                        Value::Text(text) => out.push_str(&escape(&text)),
                        Value::Bool(true) => out.push('1'),
                        Value::Bool(false) => {}
                        Value::List(_) | Value::Map(_) => {
                            return Err(at(*line, &format!("{path} is a list or map, not text")));
                        }
                    }
                }
                Node::If {
                    path,
                    not,
                    then,
                    otherwise,
                    line,
                } => {
                    let value = lookup(scope, path).map_err(|problem| at(*line, &problem))?;
                    let branch = if value.truthy() != *not {
                        then
                    } else {
                        otherwise
                    };
                    self.nodes(branch, scope, out)?;
                }
                Node::Foreach {
                    list,
                    item,
                    body,
                    line,
                } => {
                    let items = match lookup(scope, list).map_err(|problem| at(*line, &problem))? {
                        Value::List(items) => items,
                        Value::Bool(false) => Vec::new(),
                        _ => return Err(at(*line, &format!("{list} is not a list"))),
                    };
                    for value in items {
                        scope.push((item.clone(), value));
                        let result = self.nodes(body, scope, out);
                        scope.pop();
                        result?;
                    }
                }
                Node::Section { name, body } => {
                    let mut content = String::new();
                    self.nodes(body, scope, &mut content)?;
                    self.sections.entry(name.clone()).or_insert(content);
                }
                Node::Yield(name) => {
                    out.push_str(self.sections.get(name).map_or("", String::as_str))
                }
                Node::Include { name, with, line } => {
                    let source = (self.load)(name)
                        .ok_or_else(|| at(*line, &format!("included view {name} not found")))?;
                    // Evaluate every value before binding any, so `a: b, b: a` reads the caller's.
                    let mut values = Vec::with_capacity(with.len());
                    for (key, arg) in with {
                        let value = match arg {
                            Arg::Path(path) => {
                                lookup(scope, path).map_err(|problem| at(*line, &problem))?
                            }
                            Arg::Literal(value) => value.clone(),
                        };
                        values.push((key.clone(), value));
                    }
                    let count = values.len();
                    scope.extend(values);
                    let rendered = self.view(name, &source, scope);
                    scope.truncate(scope.len() - count);
                    out.push_str(&rendered?);
                }
            }
        }
        Ok(())
    }
}

fn at(line: usize, problem: &str) -> String {
    format!("line {line}: {problem}")
}

/// The value at `path` (`post.title`): the first name must be defined, a
/// missing map key is empty text.
fn lookup(scope: &[(String, Value)], path: &str) -> Result<Value, String> {
    let mut keys = path.split('.');
    let first = keys.next().unwrap_or_default();
    let mut value = scope
        .iter()
        .rev()
        .find(|(name, _)| name == first)
        .map(|(_, value)| value)
        .ok_or_else(|| format!("{first} is not defined"))?;
    for key in keys {
        let Value::Map(pairs) = value else {
            return Err(format!(
                "{path}: {key} is read from a value that is not a map"
            ));
        };
        match pairs.iter().find(|(name, _)| name == key) {
            Some((_, found)) => value = found,
            None => return Ok(Value::Text(String::new())),
        }
    }
    Ok(value.clone())
}

/// The directives, with whether each takes `(arguments)`.
const DIRECTIVES: &[(&str, bool)] = &[
    ("foreach", true),
    ("endforeach", false),
    ("if", true),
    ("else", false),
    ("endif", false),
    ("extends", true),
    ("section", true),
    ("endsection", false),
    ("yield", true),
    ("include", true),
];

enum Token {
    Text(String),
    Echo { path: String, raw: bool },
    Directive { name: &'static str, args: String },
}

/// Splits `source` into text, echoes and directives, each with its line.
fn tokenize(source: &str) -> Result<Vec<(Token, usize)>, String> {
    let mut tokens = Vec::new();
    let mut text = String::new();
    let mut line = 1;
    let mut text_line = 1;
    let mut rest = source;
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    while let Some(c) = rest.chars().next() {
        let flush = |text: &mut String, tokens: &mut Vec<(Token, usize)>, line| {
            if !text.is_empty() {
                tokens.push((Token::Text(std::mem::take(text)), line));
            }
        };
        if text.is_empty() {
            text_line = line;
        }
        if let Some(after) = rest.strip_prefix("{{--") {
            let end = after
                .find("--}}")
                .ok_or_else(|| at(line, "unclosed {{-- comment"))?;
            line += after[..end].matches('\n').count();
            rest = &after[end + 4..];
        } else if let Some(after) = rest.strip_prefix("@@") {
            text.push('@');
            rest = after;
        } else if let Some(after) = rest.strip_prefix("@{{") {
            text.push_str("{{");
            rest = after;
        } else if let Some((open, close, raw)) = [("{!!", "!!}", true), ("{{", "}}", false)]
            .into_iter()
            .find(|(open, ..)| rest.starts_with(open))
        {
            let after = &rest[open.len()..];
            let end = after
                .find(close)
                .ok_or_else(|| at(line, &format!("unclosed {open}")))?;
            let path = after[..end].trim();
            if !is_path(path) {
                return Err(at(
                    line,
                    &format!("{open} {path} {close} is not a name like post.title"),
                ));
            }
            flush(&mut text, &mut tokens, text_line);
            tokens.push((
                Token::Echo {
                    path: path.to_owned(),
                    raw,
                },
                line,
            ));
            line += after[..end].matches('\n').count();
            rest = &after[end + close.len()..];
        } else if let Some((name, takes_args, after)) =
            directive(rest).filter(|_| !text.ends_with(is_word))
        {
            let (args, after) = if takes_args {
                let Some(inside) = after.strip_prefix('(') else {
                    // `@include` in prose, not a directive.
                    text.push('@');
                    rest = &rest[1..];
                    continue;
                };
                let end =
                    closing_paren(inside).ok_or_else(|| at(line, &format!("unclosed @{name}(")))?;
                (inside[..end].to_owned(), &inside[end + 1..])
            } else {
                (String::new(), after)
            };
            flush(&mut text, &mut tokens, text_line);
            tokens.push((Token::Directive { name, args }, line));
            rest = after;
        } else {
            if c == '\n' {
                line += 1;
            }
            text.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    if !text.is_empty() {
        tokens.push((Token::Text(text), text_line));
    }
    Ok(tokens)
}

/// The directive `rest` starts with (`@if(`), whether it takes arguments, and
/// what follows its name.
fn directive(rest: &str) -> Option<(&'static str, bool, &str)> {
    let after_at = rest.strip_prefix('@')?;
    DIRECTIVES.iter().find_map(|&(name, takes_args)| {
        let after = after_at.strip_prefix(name)?;
        let boundary = !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
        boundary.then_some((name, takes_args, after))
    })
}

/// The index of the `)` closing an argument list, skipping quoted text.
fn closing_paren(inside: &str) -> Option<usize> {
    let mut quote = None;
    for (index, c) in inside.char_indices() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, ')') => return Some(index),
            (None, _) => {}
        }
    }
    None
}

fn is_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('.').all(|key| {
            !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// The text inside `'quotes'` or `"quotes"`.
fn quoted(arg: &str) -> Option<&str> {
    let arg = arg.trim();
    ['\'', '"'].into_iter().find_map(|quote| {
        arg.strip_prefix(quote)?
            .strip_suffix(quote)
            .filter(|inner| !inner.contains(quote))
    })
}

/// Splits arguments on commas outside quotes.
fn split_args(args: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut quote = None;
    let mut start = 0;
    for (index, c) in args.char_indices() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, ',') => {
                parts.push(&args[start..index]);
                start = index + 1;
            }
            (None, _) => {}
        }
    }
    parts.push(&args[start..]);
    parts
}

fn parse(source: &str) -> Result<Template, String> {
    let mut tokens = tokenize(source)?.into_iter();
    let mut extends = None;
    let (nodes, end) = block(&mut tokens, &mut extends, &[])?;
    match end {
        None => Ok(Template { extends, nodes }),
        Some((name, line)) => Err(at(line, &format!("@{name} without its opening directive"))),
    }
}

/// The directive that closed a block, and its line.
type End = (&'static str, usize);

/// Nodes up to one of `ends` (returned with its line), or to the end of the
/// view when `ends` is empty.
fn block(
    tokens: &mut impl Iterator<Item = (Token, usize)>,
    extends: &mut Option<String>,
    ends: &[&str],
) -> Result<(Vec<Node>, Option<End>), String> {
    let mut nodes = Vec::new();
    while let Some((token, line)) = tokens.next() {
        let (name, args) = match token {
            Token::Text(text) => {
                nodes.push(Node::Text(text));
                continue;
            }
            Token::Echo { path, raw } => {
                nodes.push(Node::Echo { path, raw, line });
                continue;
            }
            Token::Directive { name, args } => (name, args),
        };
        if ends.contains(&name) {
            return Ok((nodes, Some((name, line))));
        }
        let args: Vec<&str> = split_args(&args).into_iter().map(str::trim).collect();
        let view_name = |what: &str| {
            quoted(args[0])
                .filter(|name| is_view_name(name))
                .map(str::to_owned)
                .ok_or_else(|| {
                    at(
                        line,
                        &format!("@{what} needs a quoted view name like 'layouts.app'"),
                    )
                })
        };
        let expect_end = |found: Option<(&str, usize)>, end: &str| match found {
            Some(_) => Ok(()),
            None => Err(at(line, &format!("@{name} without @{end}"))),
        };
        match name {
            "if" => {
                let (not, path) = match args[0].strip_prefix('!') {
                    Some(path) => (true, path.trim()),
                    None => (false, args[0]),
                };
                if args.len() != 1 || !is_path(path) {
                    return Err(at(
                        line,
                        "@if takes one name, like @if(comments) or @if(!comments)",
                    ));
                }
                let (then, found) = block(tokens, extends, &["else", "endif"])?;
                expect_end(found, "endif")?;
                let otherwise = if found.is_some_and(|(end, _)| end == "else") {
                    let (otherwise, found) = block(tokens, extends, &["endif"])?;
                    expect_end(found, "endif")?;
                    otherwise
                } else {
                    Vec::new()
                };
                nodes.push(Node::If {
                    path: path.to_owned(),
                    not,
                    then,
                    otherwise,
                    line,
                });
            }
            "foreach" => {
                let parsed = args[0]
                    .split_once(" as ")
                    .map(|(list, item)| (list.trim(), item.trim()))
                    .filter(|(list, item)| {
                        args.len() == 1 && is_path(list) && is_path(item) && !item.contains('.')
                    });
                let Some((list, item)) = parsed else {
                    return Err(at(
                        line,
                        "@foreach takes a list and a name, like @foreach(posts as post)",
                    ));
                };
                let (body, found) = block(tokens, extends, &["endforeach"])?;
                expect_end(found, "endforeach")?;
                nodes.push(Node::Foreach {
                    list: list.to_owned(),
                    item: item.to_owned(),
                    body,
                    line,
                });
            }
            "section" => {
                let section = quoted(args[0])
                    .ok_or_else(|| at(line, "@section needs a quoted name like 'content'"))?
                    .to_owned();
                let body = match args.get(1) {
                    Some(value) if args.len() == 2 => match quoted(value) {
                        Some(text) => vec![Node::Text(escape(text))],
                        None if is_path(value) => vec![Node::Echo {
                            path: (*value).to_owned(),
                            raw: false,
                            line,
                        }],
                        None => return Err(at(line, "@section's value is quoted text or a name")),
                    },
                    Some(_) => return Err(at(line, "@section takes a name and at most one value")),
                    None => {
                        let (body, found) = block(tokens, extends, &["endsection"])?;
                        expect_end(found, "endsection")?;
                        body
                    }
                };
                nodes.push(Node::Section {
                    name: section,
                    body,
                });
            }
            "yield" => {
                let section = quoted(args[0])
                    .ok_or_else(|| at(line, "@yield needs a quoted name like 'content'"))?;
                nodes.push(Node::Yield(section.to_owned()));
            }
            "include" => {
                let mut with = Vec::new();
                for arg in &args[1..] {
                    let parsed = arg.split_once(':').and_then(|(key, value)| {
                        let (key, value) = (key.trim(), value.trim());
                        let value = match value {
                            "true" => Arg::Literal(Value::Bool(true)),
                            "false" => Arg::Literal(Value::Bool(false)),
                            _ => match quoted(value) {
                                Some(text) => Arg::Literal(Value::Text(text.to_owned())),
                                None if is_path(value) => Arg::Path(value.to_owned()),
                                None => return None,
                            },
                        };
                        (is_path(key) && !key.contains('.')).then(|| (key.to_owned(), value))
                    });
                    with.push(parsed.ok_or_else(|| {
                        at(line, "@include values look like post: featured, title: 'Hi' or compact: true")
                    })?);
                }
                nodes.push(Node::Include {
                    name: view_name("include")?,
                    with,
                    line,
                });
            }
            "extends" => *extends = Some(view_name("extends")?),
            _ => return Err(at(line, &format!("@{name} without its opening directive"))),
        }
    }
    Ok((nodes, None))
}

/// A view name: folders and a file, separated by `.` or `/`, never `..`.
pub(super) fn is_view_name(name: &str) -> bool {
    !name.is_empty()
        && name.split(['.', '/']).all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn views(files: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            files
                .iter()
                .find(|(file, _)| *file == name)
                .map(|(_, source)| (*source).to_owned())
        }
    }

    fn show(source: &str, data: &[(&str, &dyn ToValue)]) -> Result<String, String> {
        render("test", source, &Value::map(data), &views(&[]))
    }

    #[test]
    fn echoes_escape_unless_raw() {
        let body = "<b>hi</b>";
        assert_eq!(
            show("{{ body }}|{!! body !!}|{{body}}", &[("body", &body)]).unwrap(),
            "&lt;b&gt;hi&lt;/b&gt;|<b>hi</b>|&lt;b&gt;hi&lt;/b&gt;"
        );
    }

    #[test]
    fn dotted_names_read_maps_and_missing_keys_are_empty() {
        let post = Value::map(&[("title", &"Hi")]);
        assert_eq!(
            show("{{ post.title }}[{{ post.nope }}]", &[("post", &post)]).unwrap(),
            "Hi[]"
        );
    }

    #[test]
    fn unknown_names_fail_with_the_view_and_line() {
        let problem = show("a\n\n{{ heading }}", &[("title", &"x")]).unwrap_err();
        assert_eq!(problem, "view test line 3: heading is not defined");
    }

    #[test]
    fn foreach_and_if() {
        let posts = vec![
            Value::map(&[("title", &"A")]),
            Value::map(&[("title", &"B")]),
        ];
        let none: Vec<String> = Vec::new();
        let source = "<ol>@foreach(posts as post)<li>{{ post.title }}</li>@endforeach</ol>\
            @if(!posts)none @else some @endif@if(!empty)empty @endif";
        assert_eq!(
            show(source, &[("posts", &posts), ("empty", &none)]).unwrap(),
            "<ol><li>A</li><li>B</li></ol> some empty "
        );
    }

    #[test]
    fn layouts_sections_and_includes() {
        let load = views(&[
            (
                "layouts.app",
                "<title>@yield('title')</title>@include('nav')<main>@yield('content')</main>",
            ),
            ("nav", "<nav>{{ user }}</nav>"),
        ]);
        let child = "@extends('layouts.app')\n@section('title', title)\n@section('content')<p>{{ user }}</p>@endsection\n";
        let title = "A & B";
        let data = Value::map(&[("title", &title), ("user", &"neo")]);
        assert_eq!(
            render("child", child, &data, &load).unwrap(),
            "<title>A &amp; B</title><nav>neo</nav><main><p>neo</p></main>"
        );
    }

    #[test]
    fn include_passes_named_values_to_that_view_only() {
        let load = views(&[(
            "components.card",
            "<h2>{{ post.title }}</h2>@if(compact)<small>{{ label }}</small>@endif",
        )]);
        let featured = Value::map(&[("title", &"<Top>")]);
        let source = "@include('components.card', post: featured, compact: true, label: 'New')\
            @include('components.card', post: featured, compact: false, label: '')[{{ label }}]";
        let data = Value::map(&[("featured", &featured), ("label", &"outer")]);
        assert_eq!(
            render("page", source, &data, &load).unwrap(),
            "<h2>&lt;Top&gt;</h2><small>New</small><h2>&lt;Top&gt;</h2>[outer]"
        );
        let problem =
            render("page", "@include('components.card', post)", &data, &load).unwrap_err();
        assert!(problem.contains("@include values look like"), "{problem}");
    }

    #[test]
    fn css_js_and_emails_pass_through() {
        let source = "<style>@media (width<1px){a{}}</style><script>const o = { a: 1 }</script>\
            mail neo@if.com, @@if and @{{ raw }}{{-- gone --}}";
        assert_eq!(
            show(source, &[]).unwrap(),
            "<style>@media (width<1px){a{}}</style><script>const o = { a: 1 }</script>\
            mail neo@if.com, @if and {{ raw }}"
        );
    }

    #[test]
    fn broken_views_fail() {
        for (source, problem) in [
            ("@if(a)x", "line 1: @if without @endif"),
            ("x\n@endif", "line 2: @endif without its opening directive"),
            ("{{ a", "line 1: unclosed {{"),
            (
                "{{ a + b }}",
                "line 1: {{ a + b }} is not a name like post.title",
            ),
            (
                "@include('../secret')",
                "line 1: @include needs a quoted view name like 'layouts.app'",
            ),
        ] {
            assert_eq!(
                show(source, &[("a", &true)]).unwrap_err(),
                format!("view test {problem}")
            );
        }
    }

    #[test]
    fn self_including_view_stops() {
        let load = views(&[("loop", "@include('loop')")]);
        let problem = render("loop", "@include('loop')", &Value::map(&[]), &load).unwrap_err();
        assert!(problem.contains("nested more than 32"), "{problem}");
    }
}
