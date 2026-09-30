//! A client for tests that browses a [`Router`] like a visitor, without a
//! server: it keeps the cookies it is given and sends the CSRF token of the
//! last page with every form, as Laravel's `$this->get()`/`$this->post()` do.
//!
//! ```
//! use rustclamp::web::testing::Client;
//! use rustclamp::web::{Request, Response, Router};
//!
//! let app = Router::new().get("/", |_| Response::text(200, "hello"));
//! let client = Client::new(&app);
//! let page = client.get("/");
//! assert_eq!(page.status, 200);
//! assert!(page.see("hello"));
//! ```

use std::borrow::Cow;
use std::sync::Mutex;

use super::request::encode;
use super::session::CSRF_FIELD;
use super::{Request, Response, Router};

/// A visitor with a cookie jar and the CSRF token of the last page it saw.
pub struct Client<'a> {
    routes: &'a Router,
    peer: std::net::IpAddr,
    headers: Vec<(String, String)>,
    cookies: Mutex<Vec<(String, String)>>,
    token: Mutex<Option<String>>,
}

impl<'a> Client<'a> {
    /// A new visitor to `routes`, connecting from `127.0.0.1`.
    pub fn new(routes: &'a Router) -> Self {
        Self {
            routes,
            peer: [127, 0, 0, 1].into(),
            headers: Vec::new(),
            cookies: Mutex::default(),
            token: Mutex::default(),
        }
    }

    /// The same visitor connecting from `peer`, for per-client rate limits.
    ///
    /// # Panics
    ///
    /// When `peer` is not an IP address.
    #[must_use]
    pub fn from(mut self, peer: &str) -> Self {
        self.peer = peer.parse().expect("an IP address");
        self
    }

    /// The same visitor sending header `name` with every request, such as the
    /// `X-Forwarded-For` a proxy adds.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// `GET path`.
    pub fn get(&self, path: &str) -> Response {
        self.send(Request::get(path))
    }

    /// `POST path` with `fields` as a form, plus the CSRF token of the last
    /// page, as a browser submitting that page's form would.
    pub fn post(&self, path: &str, fields: &[(&str, &str)]) -> Response {
        let token = self.token.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut all: Vec<(&str, &str)> = fields.to_vec();
        if let Some(token) = &token {
            all.push((CSRF_FIELD, token));
        }
        self.post_raw(path, &all)
    }

    /// `POST path` with exactly `fields`: no CSRF token is added.
    pub fn post_raw(&self, path: &str, fields: &[(&str, &str)]) -> Response {
        let body = fields
            .iter()
            .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
            .collect::<Vec<_>>()
            .join("&");
        self.send(Request::post(path).with_body(body))
    }

    /// The value of cookie `name`, as last set by the app.
    pub fn cookie(&self, name: &str) -> Option<String> {
        let jar = self.cookies.lock().unwrap_or_else(|e| e.into_inner());
        jar.iter()
            .find(|(kept, _)| kept == name)
            .map(|(_, value)| value.clone())
    }

    fn send(&self, request: Request) -> Response {
        let jar = self
            .cookies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut request = request.with_peer(self.peer);
        for (name, value) in &self.headers {
            request = request.with_header(name, value);
        }
        if !jar.is_empty() {
            let header = jar
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ");
            request = request.with_header("Cookie", &header);
        }
        let response = self.routes.handle(&request);
        self.remember(&response);
        response
    }

    /// Keeps the response's cookies and the CSRF token of its form.
    fn remember(&self, response: &Response) {
        let mut jar = self.cookies.lock().unwrap_or_else(|e| e.into_inner());
        for (name, value) in &response.headers {
            if !name.eq_ignore_ascii_case("set-cookie") {
                continue;
            }
            let pair = value.split(';').next().unwrap_or_default();
            if let Some((cookie, value)) = pair.split_once('=') {
                jar.retain(|(kept, _)| kept != cookie.trim());
                jar.push((cookie.trim().to_owned(), value.trim().to_owned()));
            }
        }
        let marker = format!(r#"name="{CSRF_FIELD}" value=""#);
        let page = response.body_text();
        if let Some(start) = page.find(&marker) {
            let rest = &page[start + marker.len()..];
            let token = rest.split('"').next().unwrap_or_default().to_owned();
            *self.token.lock().unwrap_or_else(|e| e.into_inner()) = Some(token);
        }
    }
}

impl Response {
    /// The body as text; invalid UTF-8 is replaced.
    pub fn body_text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    /// Whether the body contains `text`, like Laravel's `assertSee` (the
    /// text as written, so escaped HTML is matched as escaped).
    pub fn see(&self, text: &str) -> bool {
        self.body_text().contains(text)
    }

    /// Where a redirect points, the `Location` header.
    pub fn location(&self) -> Option<&str> {
        self.header("location")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::{Sessions, csrf, redirect};
    use std::time::Duration;

    #[test]
    fn keeps_the_session_and_sends_the_csrf_token() {
        let app = Router::new().group(|web| {
            web.middleware(Sessions::new(Duration::from_secs(60)).middleware())
                .middleware(csrf())
                .get("/form", |request| {
                    let field = request.session().unwrap().csrf_field();
                    Response::new(200, "text/html", format!("<form>{field}</form>"))
                })
                .post("/form", |_| redirect("/done"))
        });
        let client = Client::new(&app);
        assert_eq!(client.post_raw("/form", &[("a", "1")]).status, 419);
        client.get("/form");
        let sent = client.post("/form", &[("a", "1 & 2")]);
        assert_eq!((sent.status, sent.location()), (302, Some("/done")));
        assert_eq!(
            client.post_raw("/form", &[]).status,
            419,
            "no token, refused"
        );
    }
}
