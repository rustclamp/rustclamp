//! The SQL dialects a [`Db`](super::Db) can speak (ADR 0034), the [`Sql`] a
//! migration returns, and the `?` to `$N` placeholder rewrite.

/// The SQL dialect of a database engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// SQLite: `?`/`?N` marks, `INTEGER PRIMARY KEY` ids.
    Sqlite,
    /// Postgres: `$N` marks, identity ids. Not yet reachable from a [`Db`](super::Db).
    Postgres,
}

/// The SQL of one migration step. [`Schema`](super::Schema) renders it for
/// every dialect up front; literal SQL (`"..."`.into()) runs as written on
/// whichever engine the app uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sql(Repr);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Repr {
    Raw(String),
    Both { sqlite: String, postgres: String },
}

impl Sql {
    pub(super) fn both(sqlite: String, postgres: String) -> Self {
        Self(Repr::Both { sqlite, postgres })
    }

    /// The SQL to run on `dialect`.
    pub fn render(&self, dialect: Dialect) -> &str {
        match (&self.0, dialect) {
            (Repr::Raw(sql), _) | (Repr::Both { sqlite: sql, .. }, Dialect::Sqlite) => sql,
            (Repr::Both { postgres, .. }, Dialect::Postgres) => postgres,
        }
    }
}

/// The SQLite rendering, which is what `Schema` always produced.
impl std::fmt::Display for Sql {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.render(Dialect::Sqlite))
    }
}

impl From<String> for Sql {
    fn from(sql: String) -> Self {
        Self(Repr::Raw(sql))
    }
}

impl From<&str> for Sql {
    fn from(sql: &str) -> Self {
        sql.to_owned().into()
    }
}

/// `sql` with `?` and `?N` marks written as Postgres's `$N`.
///
/// A bare `?` takes the number after the largest seen so far, as in SQLite:
/// `? ?5 ?` is `$1 $5 $6` and `?2 ? ?1` is `$2 $3 $1`. Marks inside string
/// literals (`''` escapes), quoted identifiers, `--` and nested `/* */`
/// comments, and `$$`/`$tag$` bodies are left alone, as is an unterminated one.
// ponytail: no `E'..\'..'` strings, and a jsonb `?`/`?|`/`?&` operator is
// read as a mark; write those through `jsonb_exists*` or split the statement.
pub(super) fn to_postgres_placeholders(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len() + 8);
    let (mut copied, mut at, mut largest) = (0, 0, 0u32);
    while at < bytes.len() {
        match bytes[at] {
            quote @ (b'\'' | b'"') => at = quoted_end(bytes, at, quote),
            b'-' if bytes.get(at + 1) == Some(&b'-') => {
                at += bytes[at..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .unwrap_or(bytes.len() - at);
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => at = block_comment_end(bytes, at),
            b'$' if at == 0 || !is_word(bytes[at - 1]) => at = dollar_end(bytes, at),
            b'?' => {
                let digits = bytes[at + 1..]
                    .iter()
                    .take_while(|b| b.is_ascii_digit())
                    .count();
                let number = if digits == 0 {
                    largest.saturating_add(1)
                } else {
                    sql[at + 1..at + 1 + digits].parse().unwrap_or(u32::MAX)
                };
                largest = largest.max(number);
                out.push_str(&sql[copied..at]);
                out.push('$');
                out.push_str(&number.to_string());
                at += 1 + digits;
                copied = at;
            }
            _ => at += 1,
        }
    }
    out.push_str(&sql[copied..]);
    out
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// Just past the closing `quote` of the literal opening at `from`, where a
/// doubled `quote` is an escape.
fn quoted_end(bytes: &[u8], from: usize, quote: u8) -> usize {
    let mut at = from + 1;
    while at < bytes.len() {
        if bytes[at] != quote {
            at += 1;
        } else if bytes.get(at + 1) == Some(&quote) {
            at += 2;
        } else {
            return at + 1;
        }
    }
    bytes.len()
}

/// Just past the `*/` closing the comment opening at `from`; Postgres nests.
fn block_comment_end(bytes: &[u8], from: usize) -> usize {
    let (mut depth, mut at) = (1, from + 2);
    while at < bytes.len() {
        if bytes[at..].starts_with(b"/*") {
            depth += 1;
            at += 2;
        } else if bytes[at..].starts_with(b"*/") {
            depth -= 1;
            at += 2;
            if depth == 0 {
                return at;
            }
        } else {
            at += 1;
        }
    }
    bytes.len()
}

/// Just past the closing tag of the `$tag$` body opening at `from`, or past
/// the `$` when it is not an opening tag (`$1`, a lone `$`).
fn dollar_end(bytes: &[u8], from: usize) -> usize {
    let mut tag_end = from + 1;
    while tag_end < bytes.len() && is_word(bytes[tag_end]) {
        tag_end += 1;
    }
    let digit_first = bytes.get(from + 1).is_some_and(u8::is_ascii_digit);
    if digit_first || bytes.get(tag_end) != Some(&b'$') {
        return from + 1;
    }
    let tag = &bytes[from..=tag_end];
    bytes[tag_end + 1..]
        .windows(tag.len())
        .position(|window| window == tag)
        .map_or(bytes.len(), |found| tag_end + 1 + found + tag.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg(sql: &str) -> String {
        to_postgres_placeholders(sql)
    }

    #[test]
    fn bare_and_numbered_marks() {
        assert_eq!(pg("SELECT 1"), "SELECT 1");
        assert_eq!(pg("a = ? AND b = ?"), "a = $1 AND b = $2");
        assert_eq!(
            pg("a = ?1 AND b = ?2 AND c = ?1"),
            "a = $1 AND b = $2 AND c = $1"
        );
        assert_eq!(pg("(?,?,?)"), "($1,$2,$3)");
        assert_eq!(pg("?12"), "$12");
    }

    #[test]
    fn mixed_marks_follow_sqlites_rule() {
        assert_eq!(pg("? ?5 ?"), "$1 $5 $6");
        assert_eq!(pg("?2 ? ?1"), "$2 $3 $1");
        assert_eq!(pg("?3 ?1 ?"), "$3 $1 $4");
    }

    #[test]
    fn string_literals_and_quoted_identifiers_are_skipped() {
        assert_eq!(pg("a = '?' AND b = ?"), "a = '?' AND b = $1");
        assert_eq!(pg("a = 'it''s ?' AND b = ?"), "a = 'it''s ?' AND b = $1");
        assert_eq!(
            pg(r#"SELECT "what?" FROM t WHERE x = ?"#),
            r#"SELECT "what?" FROM t WHERE x = $1"#
        );
        assert_eq!(pg(r#"SELECT "a""?" , ?"#), r#"SELECT "a""?" , $1"#);
    }

    #[test]
    fn comments_are_skipped() {
        assert_eq!(pg("a = ? -- why?\nAND b = ?"), "a = $1 -- why?\nAND b = $2");
        assert_eq!(pg("a = ? -- trailing ?"), "a = $1 -- trailing ?");
        assert_eq!(pg("/* ? */ a = ?"), "/* ? */ a = $1");
        assert_eq!(pg("/* a /* ? */ ? */ b = ?"), "/* a /* ? */ ? */ b = $1");
    }

    #[test]
    fn dollar_quoted_bodies_are_skipped() {
        assert_eq!(
            pg("DO $$ SELECT '?' ? $$ ; ?"),
            "DO $$ SELECT '?' ? $$ ; $1"
        );
        assert_eq!(pg("$fn$ ? $$ ? $fn$ ?"), "$fn$ ? $$ ? $fn$ $1");
        assert_eq!(pg("$a$ ? $b$ ? $a$ ?"), "$a$ ? $b$ ? $a$ $1");
    }

    #[test]
    fn dollar_marks_and_identifiers_pass_through() {
        assert_eq!(pg("a = $1 AND b = ?"), "a = $1 AND b = $1");
        assert_eq!(pg("SELECT 1 AS a$b, ?"), "SELECT 1 AS a$b, $1");
        assert_eq!(pg("$"), "$");
    }

    #[test]
    fn unterminated_literals_pass_through() {
        assert_eq!(pg("a = ? AND b = 'oops ?"), "a = $1 AND b = 'oops ?");
        assert_eq!(pg("a = ? /* oops ?"), "a = $1 /* oops ?");
        assert_eq!(pg("a = ? $$ oops ?"), "a = $1 $$ oops ?");
        assert_eq!(pg(r#"a = ? "oops ?"#), r#"a = $1 "oops ?"#);
    }

    #[test]
    fn multibyte_text_survives() {
        assert_eq!(
            pg("WHERE naslov = 'čšž ?' AND b = ? -- đ?\nAND c = ?"),
            "WHERE naslov = 'čšž ?' AND b = $1 -- đ?\nAND c = $2"
        );
        assert_eq!(pg("č = ?"), "č = $1");
    }
}
