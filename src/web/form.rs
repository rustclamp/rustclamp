//! Form validation, flash messages and views filled from the session, like
//! Laravel's `$request->validate()`, `back()->withErrors()` and `old()`.

use super::request::{decode, encode};
use super::session::{ERRORS, FLASH, OLD};
use super::{Request, Response, ToValue, Value, redirect, render};

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
    /// Checks form fields against rules separated by `|`: `required`,
    /// `min:N` and `max:N` (characters), `email`, `integer` and `url` (`http`
    /// or `https` only, so a stored link can never be `javascript:`). A field that
    /// is empty and not `required` passes. Values are trimmed.
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
        let mut values = Vec::new();
        let mut errors = Vec::new();
        for (name, field_rules) in rules {
            let value = self.form(name).unwrap_or_default().trim().to_owned();
            let label = name.replace('_', " ");
            let required = field_rules.split('|').any(|rule| rule == "required");
            if value.is_empty() {
                if required {
                    errors.push(format!("The {label} field is required."));
                }
            } else {
                errors.extend(
                    field_rules
                        .split('|')
                        .filter_map(|rule| check(rule, &value, &label, name)),
                );
            }
            values.push(((*name).to_owned(), value));
        }
        if errors.is_empty() {
            return Ok(Form { values });
        }
        // Passwords are never kept for refilling the form.
        let old = values
            .iter()
            .filter(|(name, _)| !name.contains("password"))
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
fn check(rule: &str, value: &str, label: &str, name: &str) -> Option<String> {
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
        _ => panic!("unknown validation rule {rule} for {name}"),
    }
}

/// An absolute `http` or `https` link with a host. Anything else, such as
/// `javascript:`, `data:` or a relative path, runs or resolves in the page
/// when put in an `href`, whatever the escaping.
fn is_web_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"));
    rest.is_some_and(|rest| {
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        !host.is_empty() && !host.starts_with('@')
    }) && !value.contains(|c: char| c.is_whitespace() || c.is_control())
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
}
