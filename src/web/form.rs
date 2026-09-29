//! Form validation, flash messages and views filled from the session, like
//! Laravel's `$request->validate()`, `back()->withErrors()` and `old()`.

use super::request::{encode, field};
use super::session::{ERRORS, FLASH, OLD};
use super::{Request, Response, escape, redirect, render};

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
    /// `min:N` and `max:N` (characters), `email` and `integer`. A field that
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

    /// [`render`] with the session filled in: `<!--csrf-->` (the CSRF field),
    /// `<!--flash-->` (the [`Session::flash`](super::Session::flash) message),
    /// `<!--errors-->` (validation errors) and `<!--old:field-->` (the input
    /// that failed, escaped). Each is shown once, then cleared; without a
    /// session they are empty. Markup uses `notice` and `notice--error`
    /// classes. `slots` are filled first, as with [`render`].
    pub fn render(&self, name: &str, slots: &[(&str, &str)]) -> Response {
        let (csrf, flash, errors, old) = match self.session() {
            Some(session) => (
                session.csrf_field(),
                session.take(FLASH),
                session.take(ERRORS),
                session.take(OLD).unwrap_or_default(),
            ),
            None => Default::default(),
        };
        let flash = flash
            .map(|message| {
                format!(
                    r#"<p class="notice" role="status">{}</p>"#,
                    escape(&message)
                )
            })
            .unwrap_or_default();
        let errors = errors
            .map(|errors| {
                let items: String = errors
                    .lines()
                    .map(|error| format!("<li>{}</li>", escape(error)))
                    .collect();
                format!(r#"<div class="notice notice--error" role="alert"><ul>{items}</ul></div>"#)
            })
            .unwrap_or_default();
        let mut all = slots.to_vec();
        all.extend([
            ("csrf", csrf.as_str()),
            ("flash", flash.as_str()),
            ("errors", errors.as_str()),
        ]);
        let mut response = render(name, &all);
        if response.status == 200 {
            let page = String::from_utf8_lossy(&response.body);
            response.body = fill_old(&page, &old).into_bytes();
        }
        response
    }
}

/// Replaces each `<!--old:field-->` with that field's escaped value from
/// `old`, or nothing.
fn fill_old(page: &str, old: &str) -> String {
    let mut out = String::with_capacity(page.len());
    let mut rest = page;
    while let Some(start) = rest.find("<!--old:") {
        out.push_str(&rest[..start]);
        let marker = &rest[start + 8..];
        let Some(end) = marker.find("-->") else {
            out.push_str(&rest[start..]);
            return out;
        };
        out.push_str(&escape(&field(old, &marker[..end]).unwrap_or_default()));
        rest = &marker[end + 3..];
    }
    out.push_str(rest);
    out
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
        _ => panic!("unknown validation rule {rule} for {name}"),
    }
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
    #[should_panic(expected = "unknown validation rule")]
    fn unknown_rule_is_a_bug() {
        let _ = Request::post("/")
            .with_body("a=1")
            .validate(&[("a", "shiny")]);
    }

    #[test]
    fn old_input_refills_escaped_and_skips_passwords() {
        let request = Request::post("/").with_body("name=%3Cb%3E&password=secret&x=");
        let invalid = request
            .validate(&[
                ("name", "max:1"),
                ("password", "required"),
                ("x", "required"),
            ])
            .unwrap_err();
        let page = fill_old(
            r#"<input value="<!--old:name-->"><input value="<!--old:password-->"><!--old:"#,
            &invalid.old,
        );
        assert_eq!(page, r#"<input value="&lt;b&gt;"><input value=""><!--old:"#);
    }

    #[test]
    fn back_without_session_is_422() {
        let request = Request::post("/").with_body("");
        let invalid = request.validate(&[("a", "required")]).unwrap_err();
        assert_eq!(invalid.back(&request, "/").status, 422);
    }
}
