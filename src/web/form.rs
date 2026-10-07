//! Form validation, flash messages and views filled from the session, like
//! Laravel's `$request->validate()`, `back()->withErrors()` and `old()`.

use super::request::{decode, encode};
use super::session::{ERRORS, FLASH, OLD};
use super::{Request, Response, ToValue, UploadedFile, Value, redirect, render};

/// Input that passed [`Request::validate`]: each field trimmed.
#[derive(Debug)]
pub struct Form {
    values: Vec<(String, String)>,
}

impl Form {
    /// The trimmed value of `field`, empty when it was optional and absent.
    ///
    /// # Panics
    ///
    /// When `field` had no rules: only validated fields can be read.
    pub fn get(&self, field: &str) -> &str {
        self.values
            .iter()
            .find(|(name, _)| name == field)
            .map(|(_, value)| value.as_str())
            .unwrap_or_else(|| panic!("form field {field} was not validated"))
    }
}

/// Input that failed [`Request::validate`].
#[derive(Debug)]
pub struct Invalid {
    /// One message per failed rule, in rule order.
    pub errors: Vec<String>,
    old: String,
}

impl Invalid {
    /// Keeps the errors and the input in the session, then redirects to
    /// `to`, where [`Request::render`] shows them once: the Post/Redirect/Get
    /// answer to a form. Without a session it answers `422` with the errors
    /// as text.
    pub fn back(self, request: &Request, to: &str) -> Response {
        let Some(session) = request.session() else {
            return Response::text(422, &self.errors.join("\n"));
        };
        session.put(ERRORS, &self.errors.join("\n"));
        session.put(OLD, &self.old);
        redirect(to)
    }
}

impl Request {
    /// A one-time message for the next page, shown by [`Request::render`] as
    /// `flash`: `session().flash(...)` without the unwrap.
    ///
    /// # Panics
    ///
    /// When the route runs without [`Sessions::middleware`](super::Sessions::middleware):
    /// the message would be lost, which is a bug in the app.
    pub fn flash(&self, message: &str) {
        self.session()
            .expect("flash needs Sessions::middleware on this route")
            .flash(message);
    }

    /// Checks form fields against rules separated by `|`: `required`,
    /// `min:N` and `max:N` (characters), `email`, `integer`, `numeric`,
    /// `date` (`YYYY-MM-DD`, what `<input type="date">` sends), `in:a,b,c`,
    /// `confirmed` (equal to the `{field}_confirmation` field) and `url` (`http`
    /// or `https` only, so a stored link can never be `javascript:`). With the
    /// `db` feature, `unique:table,column` fails when a row already has the value,
    /// and `unique:table,column,ID` ignores the row with that `id` (an edit form);
    /// it reads the [`Db`](crate::db::Db) given to [`Router::state`](super::Router::state).
    /// A field that is empty and not `required` passes. Values are trimmed.
    ///
    /// A field with `file`, `image` or `mimes:...` is an upload
    /// ([`Request::file`]): `image` is a JPEG, PNG, GIF or WebP by extension
    /// and by content, `mimes:pdf,txt` limits the extension, and `min:N`/`max:N`
    /// count kilobytes. Its value in the [`Form`] is the name the browser sent.
    ///
    /// ```
    /// use rustclamp::web::Request;
    ///
    /// let request = Request::post("/contact").with_body("name=+Neo+&email=nope");
    /// let rules = [("name", "required|max:100"), ("email", "required|email")];
    /// let invalid = request.validate(&rules).unwrap_err();
    /// assert_eq!(invalid.errors, ["The email field must be a valid email address."]);
    ///
    /// let request = Request::post("/contact").with_body("name=Neo&email=n%40x.si");
    /// assert_eq!(request.validate(&rules).unwrap().get("name"), "Neo");
    /// ```
    ///
    /// # Panics
    ///
    /// On a rule it does not know: that is a bug in the app, not bad input.
    pub fn validate(&self, rules: &[(&str, &str)]) -> Result<Form, Invalid> {
        self.validate_with(rules, &[])
    }

    /// [`validate`](Self::validate) with your own messages, keyed
    /// `field.rule`; rules without one keep the default message.
    ///
    /// ```
    /// use rustclamp::web::Request;
    ///
    /// let request = Request::post("/").with_body("plan=gold");
    /// let invalid = request
    ///     .validate_with(&[("plan", "in:free,pro")], &[("plan.in", "Pick a plan.")])
    ///     .unwrap_err();
    /// assert_eq!(invalid.errors, ["Pick a plan."]);
    /// ```
    ///
    /// # Panics
    ///
    /// As [`validate`](Self::validate).
    pub fn validate_with(
        &self,
        rules: &[(&str, &str)],
        messages: &[(&str, &str)],
    ) -> Result<Form, Invalid> {
        let message = |name: &str, rule: &str, default: String| {
            let rule = rule.split_once(':').map_or(rule, |(rule, _)| rule);
            messages
                .iter()
                .find(|(key, _)| key.rsplit_once('.') == Some((name, rule)))
                .map_or(default, |(_, custom)| (*custom).to_owned())
        };
        let mut values = Vec::new();
        let mut errors = Vec::new();
        let mut files = Vec::new();
        for (name, field_rules) in rules {
            let label = name.replace('_', " ");
            let required = field_rules.split('|').any(|rule| rule == "required");
            let is_file = field_rules
                .split('|')
                .any(|rule| matches!(rule, "file" | "image") || rule.starts_with("mimes:"));
            if is_file {
                match self.file(name) {
                    None if required => errors.push(message(
                        name,
                        "required",
                        format!("The {label} field is required."),
                    )),
                    None => {}
                    Some(file) => errors.extend(field_rules.split('|').filter_map(|rule| {
                        check_file(rule, &file, &label, name).map(|m| message(name, rule, m))
                    })),
                }
                let sent = self.file(name).map(|file| file.name).unwrap_or_default();
                values.push(((*name).to_owned(), sent));
                files.push(*name);
                continue;
            }
            let value = self.form(name).unwrap_or_default().trim().to_owned();
            if value.is_empty() {
                if required {
                    errors.push(message(
                        name,
                        "required",
                        format!("The {label} field is required."),
                    ));
                }
            } else {
                errors.extend(field_rules.split('|').filter_map(|rule| {
                    check(self, rule, &value, &label, name).map(|m| message(name, rule, m))
                }));
            }
            values.push(((*name).to_owned(), value));
        }
        if errors.is_empty() {
            return Ok(Form { values });
        }
        // Passwords are never kept for refilling the form, and a browser
        // cannot refill a file input.
        let old = values
            .iter()
            .filter(|(name, _)| !name.contains("password") && !files.contains(&name.as_str()))
            .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
            .collect::<Vec<_>>()
            .join("&");
        Err(Invalid { errors, old })
    }

    /// [`render`] with the session's form state added to `data`: `csrf` (the
    /// CSRF field, print it with `{!! csrf !!}`), `flash` (the
    /// [`Session::flash`](super::Session::flash) message, or `false`),
    /// `errors` (the validation errors, a list) and `old` (the input that
    /// failed, `{{ old.email }}`). Each is shown once, then cleared; without a
    /// session they are empty.
    pub fn render(&self, name: &str, data: &[(&str, &dyn ToValue)]) -> Response {
        let (csrf, flash, errors, old) = match self.session() {
            Some(session) => (
                session.csrf_field(),
                session.take(FLASH),
                session.take(ERRORS).unwrap_or_default(),
                session.take(OLD).unwrap_or_default(),
            ),
            None => Default::default(),
        };
        let errors: Vec<&str> = errors.lines().collect();
        let old = Value::Map(
            old.split('&')
                .filter(|pair| !pair.is_empty())
                .map(|pair| {
                    let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                    (decode(name), Value::Text(decode(value)))
                })
                .collect(),
        );
        let mut all = data.to_vec();
        all.extend([
            ("csrf", &csrf as &dyn ToValue),
            ("flash", &flash),
            ("errors", &errors),
            ("old", &old),
        ]);
        render(name, &all)
    }
}

/// The message for `value` failing `rule`, if it does.
fn check(request: &Request, rule: &str, value: &str, label: &str, name: &str) -> Option<String> {
    let (rule, argument) = rule.split_once(':').unwrap_or((rule, ""));
    let limit = || {
        argument
            .parse::<usize>()
            .unwrap_or_else(|_| panic!("rule {rule} for {name} needs a number"))
    };
    let length = value.chars().count();
    match rule {
        "required" => None,
        "max" => (length > limit()).then(|| {
            format!(
                "The {label} field must not be longer than {} characters.",
                limit()
            )
        }),
        "min" => (length < limit())
            .then(|| format!("The {label} field must be at least {} characters.", limit())),
        "email" => {
            // ponytail: shape check only; verify by sending mail when it matters
            let valid = !value.contains(char::is_whitespace)
                && value.split_once('@').is_some_and(|(user, domain)| {
                    !user.is_empty()
                        && domain.contains('.')
                        && !domain.starts_with('.')
                        && !domain.ends_with('.')
                });
            (!valid).then(|| format!("The {label} field must be a valid email address."))
        }
        "integer" => value
            .parse::<i64>()
            .is_err()
            .then(|| format!("The {label} field must be a whole number.")),
        "url" => (!is_web_url(value)).then(|| {
            format!("The {label} field must be a link starting with http:// or https://.")
        }),
        "numeric" => (!value.parse::<f64>().is_ok_and(f64::is_finite))
            .then(|| format!("The {label} field must be a number.")),
        "date" => crate::time::parse_date(value)
            .is_none()
            .then(|| format!("The {label} field must be a valid date (YYYY-MM-DD).")),
        "in" => (!argument.split(',').any(|allowed| allowed == value))
            .then(|| format!("The selected {label} is invalid.")),
        "confirmed" => {
            let confirmation = request.form(&format!("{name}_confirmation"));
            (confirmation.unwrap_or_default().trim() != value)
                .then(|| format!("The {label} field confirmation does not match."))
        }
        #[cfg(feature = "db")]
        "unique" => {
            let db = request
                .state::<crate::db::Db>()
                .unwrap_or_else(|| panic!("rule unique for {name} needs a Db in Router::state"));
            let mut parts = argument.split(',');
            let table = parts.next().filter(|table| !table.is_empty());
            let table = table.unwrap_or_else(|| panic!("rule unique for {name} needs a table"));
            let column = parts.next().unwrap_or(name);
            let except = parts.next();
            let mut query = db.table(table).where_eq(column, &value);
            if let Some(id) = &except {
                query = query.where_op("id", "!=", id);
            }
            // A database error is not bad input: it panics into a 500.
            let taken = query
                .count()
                .unwrap_or_else(|error| panic!("rule unique for {name}: {error}"));
            (taken > 0).then(|| format!("The {label} has already been taken."))
        }
        _ => panic!("unknown validation rule {rule} for {name}"),
    }
}

/// One rule for an uploaded file. Sizes are kilobytes, as in Laravel.
fn check_file(rule: &str, file: &UploadedFile, label: &str, name: &str) -> Option<String> {
    let (rule, argument) = rule.split_once(':').unwrap_or((rule, ""));
    let kilobytes = || {
        argument
            .parse::<usize>()
            .unwrap_or_else(|_| panic!("rule {rule} for {name} needs a number"))
    };
    let extension = file.extension().unwrap_or_default();
    match rule {
        "required" | "file" => None,
        "max" => (file.size() > kilobytes() * 1024).then(|| {
            format!(
                "The {label} field must not be greater than {} kilobytes.",
                kilobytes()
            )
        }),
        "min" => (file.size() < kilobytes() * 1024).then(|| {
            format!(
                "The {label} field must be at least {} kilobytes.",
                kilobytes()
            )
        }),
        "mimes" => (!argument
            .split(',')
            .any(|allowed| allowed.trim().eq_ignore_ascii_case(&extension)))
        .then(|| format!("The {label} field must be a file of type: {argument}.")),
        "image" => {
            // The content must match the extension: a renamed page is not an
            // image. SVG is left out on purpose: it can carry script.
            let bytes = &file.bytes;
            let valid = match extension.as_str() {
                "jpg" | "jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
                "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
                "webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
                _ => false,
            };
            (!valid).then(|| format!("The {label} field must be an image."))
        }
        _ => panic!("unknown validation rule {rule} for file field {name}"),
    }
}

/// An absolute `http` or `https` link with a host. Anything else, such as
/// `javascript:`, `data:` or a relative path, runs or resolves in the page
/// when put in an `href`, whatever the escaping.
fn is_web_url(value: &str) -> bool {
    let web = matches!(url_scheme(value).as_deref(), Some("http" | "https"));
    let host = value.split_once("://").map_or("", |(_, rest)| {
        rest.split(['/', '?', '#']).next().unwrap_or_default()
    });
    web && !host.is_empty()
        && !host.starts_with('@')
        && !value.contains(|c: char| c.is_whitespace() || c.is_control())
}

/// The lowercased scheme of `url` (`javascript` in `JavaScript:x`), or `None`
/// for a relative link: the part before the first `:`, if no `/`, `?` or `#`
/// comes first.
pub(super) fn url_scheme(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once(':')?;
    (!scheme.contains(['/', '?', '#'])).then(|| scheme.trim().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_and_messages() {
        let request = Request::post("/").with_body("name=&bio=abcdef&age=x&note=&mail=a%40b");
        let invalid = request
            .validate(&[
                ("name", "required"),
                ("bio", "max:3"),
                ("age", "integer"),
                ("note", "min:5"),
                ("mail", "email"),
            ])
            .unwrap_err();
        assert_eq!(
            invalid.errors,
            [
                "The name field is required.",
                "The bio field must not be longer than 3 characters.",
                "The age field must be a whole number.",
                "The mail field must be a valid email address.",
            ],
            "an empty optional field skips its rules"
        );
    }

    #[test]
    fn url_accepts_only_web_links() {
        for good in [
            "https://rustclamp.com",
            "http://a.si/x?y=1#z",
            "HTTPS://A.SI",
        ] {
            assert!(is_web_url(good), "{good}");
        }
        for bad in [
            "javascript:alert(1)",
            "JavaScript://%0aalert(1)",
            "data:text/html,x",
            "//evil.com",
            "/relative",
            "https://",
            "https:// evil.com",
            "https://a.si/\nx\u{7}",
        ] {
            assert!(!is_web_url(bad), "{bad}");
        }
        let invalid = Request::post("/")
            .with_body("site=javascript%3Aalert(1)")
            .validate(&[("site", "url")])
            .unwrap_err();
        assert_eq!(
            invalid.errors,
            ["The site field must be a link starting with http:// or https://."]
        );
    }

    #[test]
    fn numeric_date_in_and_confirmed() {
        let request = Request::post("/").with_body(
            "price=1.5&bad_price=inf&day=2026-02-29&plan=gold&password=a&password_confirmation=b",
        );
        let invalid = request
            .validate(&[
                ("price", "numeric"),
                ("bad_price", "numeric"),
                ("day", "date"),
                ("plan", "in:free,pro"),
                ("password", "confirmed"),
            ])
            .unwrap_err();
        assert_eq!(
            invalid.errors,
            [
                "The bad price field must be a number.",
                "The day field must be a valid date (YYYY-MM-DD).",
                "The selected plan is invalid.",
                "The password field confirmation does not match.",
            ]
        );
        let ok = Request::post("/").with_body("day=2028-02-29&plan=pro&pin=1&pin_confirmation=1");
        assert!(
            ok.validate(&[
                ("day", "date"),
                ("plan", "in:free,pro"),
                ("pin", "confirmed")
            ])
            .is_ok()
        );
    }

    #[test]
    fn custom_messages_by_field_and_rule() {
        let request = Request::post("/").with_body("email=x");
        let invalid = request
            .validate_with(
                &[("name", "required"), ("email", "email|min:3")],
                &[
                    ("name.required", "Who are you?"),
                    ("email.min", "Too short."),
                ],
            )
            .unwrap_err();
        assert_eq!(
            invalid.errors,
            [
                "Who are you?",
                "The email field must be a valid email address.",
                "Too short."
            ]
        );
    }

    #[cfg(feature = "db")]
    #[test]
    fn unique_reads_the_router_db() {
        use super::super::request::State;
        use std::sync::Arc;
        let db = crate::db::Db::open(&crate::config::Config::parse("DB_DATABASE=:memory:"));
        db.with(|sql| {
            sql.execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT);
                 INSERT INTO users (id, email) VALUES (1, 'a@b.si');",
            )
        })
        .unwrap();
        let state = State(Arc::new(vec![Arc::new(db)]));
        let mut request = Request::post("/").with_body("email=a%40b.si");
        request.state = state;
        let invalid = request.validate(&[("email", "unique:users")]).unwrap_err();
        assert_eq!(invalid.errors, ["The email has already been taken."]);
        assert!(
            request
                .validate(&[("email", "unique:users,email,1")])
                .is_ok()
        );
        assert!(
            request
                .validate(&[("email", "unique:users,email,2")])
                .is_err()
        );
    }

    #[test]
    #[should_panic(expected = "unknown validation rule")]
    fn unknown_rule_is_a_bug() {
        let _ = Request::post("/")
            .with_body("a=1")
            .validate(&[("a", "shiny")]);
    }

    #[test]
    fn old_input_skips_passwords() {
        let request = Request::post("/").with_body("name=%3Cb%3E&password=secret&x=");
        let invalid = request
            .validate(&[
                ("name", "max:1"),
                ("password", "required"),
                ("x", "required"),
            ])
            .unwrap_err();
        assert_eq!(invalid.old, "name=%3Cb%3E&x=");
    }

    #[test]
    fn back_without_session_is_422() {
        let request = Request::post("/").with_body("");
        let invalid = request.validate(&[("a", "required")]).unwrap_err();
        assert_eq!(invalid.back(&request, "/").status, 422);
    }

    fn upload(parts: &[(&str, &str, &[u8])]) -> Request {
        let mut body = Vec::new();
        for (name, filename, bytes) in parts {
            body.extend_from_slice(b"--b\r\nContent-Disposition: form-data; name=\"");
            body.extend_from_slice(name.as_bytes());
            if !filename.is_empty() {
                body.extend_from_slice(b"\"; filename=\"");
                body.extend_from_slice(filename.as_bytes());
            }
            body.extend_from_slice(b"\"\r\n\r\n");
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(b"--b--\r\n");
        Request::post("/")
            .with_header("content-type", "multipart/form-data; boundary=b")
            .with_body(body)
    }

    #[test]
    fn validates_uploads() {
        let png: &[u8] = b"\x89PNG\r\n\x1a\nrest";
        let rules = [
            ("title", "required|max:5"),
            ("photo", "required|image|max:1"),
            ("doc", "mimes:pdf,txt"),
        ];
        let ok = upload(&[("title", "", b"Beach"), ("photo", "a.PNG", png)]);
        let form = ok.validate(&rules).unwrap();
        assert_eq!(form.get("photo"), "a.PNG");
        assert_eq!(form.get("doc"), "");

        // A page renamed to .png, a file over 1 KB, a wrong type, a missing
        // required file; the text field's max still counts characters.
        let bad = upload(&[
            ("title", "", b"Beaches"),
            ("photo", "x.png", b"<script>"),
            ("doc", "d.exe", b"MZ"),
        ]);
        assert_eq!(
            bad.validate(&rules).unwrap_err().errors,
            [
                "The title field must not be longer than 5 characters.",
                "The photo field must be an image.",
                "The doc field must be a file of type: pdf,txt.",
            ]
        );
        let big = [png, &[0; 1024]].concat();
        let invalid = upload(&[("title", "", b"a"), ("photo", "a.png", &big)])
            .validate(&rules)
            .unwrap_err();
        assert_eq!(
            invalid.errors,
            ["The photo field must not be greater than 1 kilobytes."]
        );
        let invalid = upload(&[("title", "", b"a")]).validate(&rules).unwrap_err();
        assert_eq!(invalid.errors, ["The photo field is required."]);
        assert!(!invalid.old.contains("photo"));
        let svg = upload(&[("title", "", b"a"), ("photo", "a.svg", b"<svg/>")]);
        assert!(svg.validate(&rules).is_err());
    }
}
