//! A blog on `rustclamp::web`: public posts, an admin area behind login and
//! roles, and a contact form that mails the site owner (phase 11).
//!
//! ```text
//! cargo run --example blog --features auth,mail -- user:create you@example.com super-admin
//! cargo run --example blog --features auth,mail
//! ```
//!
//! Admins publish and archive posts at `/admin/posts`; only a super-admin
//! changes roles at `/admin/users`. Blocking someone is a role change, and it
//! applies on their next request. The contact form goes through all three
//! mail layers: 3 sends a minute per client, the outbox, and `MAIL_DAILY_CAP`.

use std::time::Duration;

use rustclamp::config::Config;
use rustclamp::db::{Db, Migration, Schema, Seeder, Sql};
use rustclamp::mail::Message;
use rustclamp::uuid::Uuid;
use rustclamp::web::auth::{self, Refused};
use rustclamp::web::{
    self, App, Request, Response, Router, Sessions, csrf, escape, redirect, throttle,
};

/// Highest first. `editor` is the blog's own role: it may write drafts but
/// not publish them.
const ROLES: &[&str] = &["super-admin", "admin", "editor", "user", "blocked"];

struct CreateUsers;

impl Migration for CreateUsers {
    fn name(&self) -> &'static str {
        "0001_create_users"
    }
    fn up(&self) -> Sql {
        Schema::create("users", |table| {
            table.id();
            table.public_id();
            table.string("name");
            table.string("email").unique();
            table.string("password");
            table.string("role").default("'user'");
            table.timestamp("email_verified_at").nullable();
            table.timestamps();
        })
    }
    fn down(&self) -> Sql {
        Schema::drop("users")
    }
}

struct CreatePosts;

impl Migration for CreatePosts {
    fn name(&self) -> &'static str {
        "0002_create_posts"
    }
    fn up(&self) -> Sql {
        Schema::create("posts", |table| {
            table.id();
            table.public_id();
            table.string("title");
            table.text("body");
            table.string("status").default("'draft'");
            table.timestamps();
        })
    }
    fn down(&self) -> Sql {
        Schema::drop("posts")
    }
}

fn migrations() -> Vec<&'static dyn Migration> {
    vec![&CreateUsers, &CreatePosts]
}

fn seeders() -> Vec<&'static dyn Seeder> {
    Vec::new()
}

fn filesystems(_: &Config) -> rustclamp::storage::Settings {
    rustclamp::storage::Settings {
        default: "local".into(),
        disks: [("local".into(), rustclamp::storage::Disk::new("storage/app"))].into(),
        links: Vec::new(),
    }
}

fn routes(router: Router, _: &Config, _: &Db) -> Router {
    router.group(|site| {
        site.middleware(Sessions::new(Duration::from_secs(2 * 60 * 60)).middleware())
            .middleware(csrf())
            .middleware(auth::authenticate)
            .get("/", home)
            .get("/login", login_form)
            .post("/login", login)
            .post("/logout", |request| {
                auth::logout(request);
                redirect("/")
            })
            .get("/contact", contact_form)
            .group(|sending| {
                // The first mail layer; the outbox and daily cap follow in `Mailer`.
                sending
                    .middleware(throttle(3, Duration::from_secs(60)))
                    .post("/contact", contact)
            })
            .group(|editors| {
                editors
                    .middleware(auth::role("editor"))
                    .get("/admin/posts", admin_posts)
                    .post("/admin/posts", create_post)
                    .post("/admin/posts/{post}/{action}", transition_post)
            })
            .group(|owners| {
                owners
                    .middleware(auth::role("super-admin"))
                    .get("/admin/users", admin_users)
                    .post("/admin/users/{user}/role", change_role)
            })
    })
}

/// The app, for `main` and the tests.
fn app() -> App {
    App {
        logging: rustclamp::log::Settings::from_config,
        database: rustclamp::db::Settings::from_config,
        filesystems,
        migrations,
        seeders,
        routes,
        commands: &[],
        roles: ROLES,
        mail: rustclamp::mail::Settings::from_config,
        #[cfg(feature = "queue")]
        jobs: Vec::new,
        #[cfg(feature = "schedule")]
        schedule: &[],
    }
}

fn main() {
    app().run();
}

fn page(request: &Request, title: &str, body: &str) -> Response {
    let nav = match request.user() {
        Some(user) => format!(
            "<form method=post action=/logout>{}{} <button>Log out</button></form>",
            csrf_field(request),
            escape(&user.name)
        ),
        None => "<a href=/login>Log in</a>".to_owned(),
    };
    Response::new(
        200,
        "text/html; charset=utf-8",
        format!(
            "<!doctype html><meta charset=utf-8><title>{0}</title>\
             <nav><a href=/>Blog</a> <a href=/contact>Contact</a> {nav}</nav><h1>{0}</h1>{body}",
            escape(title)
        ),
    )
}

fn csrf_field(request: &Request) -> String {
    request
        .session()
        .map(|session| session.csrf_field())
        .unwrap_or_default()
}

fn home(request: &Request) -> web::Result {
    let posts = request
        .db()
        .table("posts")
        .where_eq("status", &"published")
        .order_by_desc("id")
        .get(|row| Ok((row.get::<String>("title")?, row.get::<String>("body")?)))?;
    let body: String = posts
        .iter()
        .map(|(title, body)| {
            format!(
                "<article><h2>{}</h2><p>{}</p></article>",
                escape(title),
                escape(body)
            )
        })
        .collect();
    Ok(page(request, "Blog", &body))
}

fn login_form(request: &Request) -> Response {
    page(
        request,
        "Log in",
        &format!(
            "<form method=post>{}<input name=email type=email> \
             <input name=password type=password> <button>Log in</button></form>",
            csrf_field(request)
        ),
    )
}

fn login(request: &Request) -> web::Result {
    let email = request.form("email").unwrap_or_default();
    let password = request.form("password").unwrap_or_default();
    Ok(match request.auth().attempt(request, &email, &password)? {
        Ok(_) => redirect("/admin/posts"),
        Err(Refused::Throttled(_)) => Response::text(429, "Too many attempts; try again later."),
        // Wrong password, unknown email and blocked look alike to the visitor.
        Err(_) => Response::text(422, "These credentials do not match our records."),
    })
}

fn contact_form(request: &Request) -> Response {
    page(
        request,
        "Contact",
        &format!(
            "<form method=post>{}<input name=email type=email> \
             <textarea name=message></textarea> <button>Send</button></form>",
            csrf_field(request)
        ),
    )
}

/// Mails the site owner: the visitor's address goes in the text, never in
/// the recipient, so the form cannot send mail anywhere else.
fn contact(request: &Request) -> web::Result {
    let message = request.form("message").unwrap_or_default();
    if message.trim().is_empty() {
        return Ok(Response::text(422, "Write a message."));
    }
    let Some(owner) = request.mailer().admin() else {
        return Ok(Response::text(503, "The contact form is not set up."));
    };
    let from = request.form("email").unwrap_or_default();
    let text = format!("From: {from}\n\n{message}");
    request
        .mailer()
        .send(request.db(), Message::new(owner, "Contact form", &text))?;
    Ok(page(
        request,
        "Thanks",
        "<p>Your message is on its way.</p>",
    ))
}

fn admin_posts(request: &Request) -> web::Result {
    let posts = request.db().table("posts").order_by_desc("id").get(|row| {
        Ok((
            row.get::<String>("public_id")?,
            row.get::<String>("title")?,
            row.get::<String>("status")?,
        ))
    })?;
    let token = csrf_field(request);
    let can_publish = request
        .user()
        .is_some_and(|user| request.auth().at_least(user, "admin"));
    let rows: String = posts
        .iter()
        .map(|(id, title, status)| {
            let actions = if can_publish {
                format!(
                    "<form method=post action=/admin/posts/{id}/publish>{token}<button>Publish</button></form>\
                     <form method=post action=/admin/posts/{id}/archive>{token}<button>Archive</button></form>"
                )
            } else {
                String::new()
            };
            format!("<li>{} ({status}) {actions}</li>", escape(title))
        })
        .collect();
    Ok(page(
        request,
        "Posts",
        &format!(
            "<ul>{rows}</ul><form method=post>{token}<input name=title> \
             <textarea name=body></textarea> <button>Save draft</button></form>"
        ),
    ))
}

fn create_post(request: &Request) -> web::Result {
    let title = request.form("title").unwrap_or_default();
    if title.trim().is_empty() {
        return Ok(Response::text(422, "A post needs a title."));
    }
    let body = request.form("body").unwrap_or_default();
    request.db().table("posts").insert(
        &["public_id", "title", "body"],
        &[&Uuid::v7().to_string(), &title.trim(), &body],
    )?;
    Ok(redirect("/admin/posts"))
}

/// Publishing and archiving need `admin`; an editor gets the same `404` as
/// a missing post.
fn transition_post(request: &Request) -> web::Result {
    let user = request.user().expect("behind auth::role");
    let status = match request.param("action") {
        Some("publish") => "published",
        Some("archive") => "archived",
        _ => return Ok(web::error(404)),
    };
    if !request.auth().at_least(user, "admin") {
        return Ok(web::error(403));
    }
    let post = request.param("post").unwrap_or_default();
    let changed = request
        .db()
        .table("posts")
        .where_eq("public_id", &post)
        .update(&["status"], &[&status])?;
    Ok(if changed == 0 {
        web::error(404)
    } else {
        redirect("/admin/posts")
    })
}

fn admin_users(request: &Request) -> web::Result {
    let users = request.db().table("users").order_by("id").get(|row| {
        Ok((
            row.get::<String>("public_id")?,
            row.get::<String>("email")?,
            row.get::<String>("role")?,
        ))
    })?;
    let token = csrf_field(request);
    let options = |held: &str| -> String {
        ROLES
            .iter()
            .map(|role| {
                let selected = if *role == held { " selected" } else { "" };
                format!("<option{selected}>{role}</option>")
            })
            .collect()
    };
    let rows: String = users
        .iter()
        .map(|(id, email, role)| {
            format!(
                "<li>{} <form method=post action=/admin/users/{id}/role>{token}\
                 <select name=role>{}</select> <button>Save</button></form></li>",
                escape(email),
                options(role)
            )
        })
        .collect();
    Ok(page(request, "Users", &format!("<ul>{rows}</ul>")))
}

fn change_role(request: &Request) -> web::Result {
    let public_id = request.param("user").unwrap_or_default();
    let Some(id) = request
        .db()
        .table("users")
        .where_eq("public_id", &public_id)
        .first(|row| row.get::<i64>("id"))?
    else {
        return Ok(web::error(404));
    };
    let role = request.form("role").unwrap_or_default();
    if !ROLES.contains(&role.as_str()) {
        return Ok(Response::text(422, "Unknown role."));
    }
    request.auth().sync_role(request.db(), id, &role)?;
    Ok(redirect("/admin/users"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustclamp::web::testing::Client;

    const ENV: &str = "MAIL_ADMIN_ADDRESS=owner@blog.test\nMAIL_DAILY_CAP=2\n";

    /// The blog with `root` (super-admin), `ada` (admin), `ed` (editor),
    /// `bob` (user) and `eve` (blocked), password `secret-pass`.
    fn blog() -> (Router, Db) {
        let (routes, db) = app().test(ENV);
        let auth = auth::Auth::new(ROLES);
        for (name, role) in [
            ("root", "super-admin"),
            ("ada", "admin"),
            ("ed", "editor"),
            ("bob", "user"),
            ("eve", "blocked"),
        ] {
            auth.create_user(&db, name, &format!("{name}@blog.test"), "secret-pass", role)
                .unwrap();
        }
        (routes, db)
    }

    fn logged_in<'a>(routes: &'a Router, who: &str) -> Client<'a> {
        let client = Client::new(routes);
        client.get("/login");
        let response = client.post(
            "/login",
            &[
                ("email", &format!("{who}@blog.test")),
                ("password", "secret-pass"),
            ],
        );
        assert_eq!(response.location(), Some("/admin/posts"), "{who} logs in");
        // Login moves the session, so take the new CSRF token from a page.
        client.get("/contact");
        client
    }

    fn post_id(db: &Db, title: &str) -> String {
        db.table("posts")
            .where_eq("title", &title)
            .first(|row| row.get::<String>("public_id"))
            .unwrap()
            .expect("the post exists")
    }

    fn user_id(db: &Db, name: &str) -> (i64, String) {
        db.table("users")
            .where_eq("name", &name)
            .first(|row| Ok((row.get::<i64>("id")?, row.get::<String>("public_id")?)))
            .unwrap()
            .expect("the user exists")
    }

    #[test]
    fn guests_go_to_login_and_403_is_404() {
        let (routes, _) = blog();
        let guest = Client::new(&routes);
        assert_eq!(guest.get("/admin/posts").location(), Some("/login"));
        assert_eq!(guest.get("/admin/users").location(), Some("/login"));

        let bob = logged_in(&routes, "bob");
        assert_eq!(
            bob.get("/admin/posts").status,
            404,
            "a user is not an editor"
        );
        let ada = logged_in(&routes, "ada");
        assert_eq!(ada.get("/admin/posts").status, 200);
        assert_eq!(
            ada.get("/admin/users").status,
            404,
            "only a super-admin changes roles"
        );
        assert_eq!(logged_in(&routes, "root").get("/admin/users").status, 200);
    }

    #[test]
    fn editors_write_and_admins_publish() {
        let (routes, db) = blog();
        let ed = logged_in(&routes, "ed");
        ed.post(
            "/admin/posts",
            &[("title", "Hello"), ("body", "First post")],
        );
        let id = post_id(&db, "Hello");
        assert!(
            !Client::new(&routes).get("/").see("First post"),
            "drafts stay hidden"
        );

        let publish = format!("/admin/posts/{id}/publish");
        assert_eq!(
            ed.post(&publish, &[]).status,
            404,
            "an editor cannot publish"
        );
        let ada = logged_in(&routes, "ada");
        assert_eq!(ada.post(&publish, &[]).location(), Some("/admin/posts"));
        assert!(Client::new(&routes).get("/").see("First post"));

        ada.post(&format!("/admin/posts/{id}/archive"), &[]);
        assert!(!Client::new(&routes).get("/").see("First post"));
        assert_eq!(ada.post("/admin/posts/missing/publish", &[]).status, 404);
    }

    #[test]
    fn blocked_users_cannot_log_in_or_stay_logged_in() {
        let (routes, db) = blog();
        let eve = Client::new(&routes);
        eve.get("/login");
        let refused = eve.post(
            "/login",
            &[("email", "eve@blog.test"), ("password", "secret-pass")],
        );
        assert_eq!(refused.status, 422);
        assert_eq!(eve.get("/admin/posts").location(), Some("/login"));

        let ada = logged_in(&routes, "ada");
        let root = logged_in(&routes, "root");
        let (_, ada_public) = user_id(&db, "ada");
        root.post(
            &format!("/admin/users/{ada_public}/role"),
            &[("role", "blocked")],
        );
        assert_eq!(
            ada.get("/admin/posts").location(),
            Some("/login"),
            "blocking ends the session on the next request"
        );
    }

    #[test]
    fn role_changes_apply_on_the_next_request() {
        let (routes, db) = blog();
        let bob = logged_in(&routes, "bob");
        let root = logged_in(&routes, "root");
        let (bob_id, bob_public) = user_id(&db, "bob");
        let change = format!("/admin/users/{bob_public}/role");

        root.post(&change, &[("role", "admin")]);
        assert_eq!(bob.get("/admin/posts").status, 200, "promoted");
        root.post(&change, &[("role", "user")]);
        assert_eq!(bob.get("/admin/posts").status, 404, "demoted");

        assert_eq!(root.post(&change, &[("role", "pope")]).status, 422);
        let role: String = db
            .table("users")
            .where_eq("id", &bob_id)
            .first(|row| row.get("role"))
            .unwrap()
            .unwrap();
        assert_eq!(role, "user", "an unknown role changes nothing");
        // Every user's role is one of the app's roles (the house test).
        let roles = db
            .table("users")
            .get(|row| row.get::<String>("role"))
            .unwrap();
        assert!(roles.iter().all(|role| ROLES.contains(&role.as_str())));
    }

    #[test]
    fn contact_mail_passes_all_three_layers() {
        let (routes, db) = blog();
        let visitor = Client::new(&routes).from("203.0.113.9");
        visitor.get("/contact");
        let send = |text: &str| {
            visitor.post(
                "/contact",
                &[("email", "visitor@else.test"), ("message", text)],
            )
        };
        for n in 1..=3 {
            assert_eq!(send(&format!("message {n}")).status, 200, "message {n}");
        }
        assert_eq!(
            send("message 4").status,
            429,
            "layer 1: 3 a minute per client"
        );

        // Layer 2: every send waits in the outbox, addressed to the owner.
        let mailer = rustclamp::mail::Mailer::new(rustclamp::mail::Settings::from_config(
            &Config::parse(ENV),
        ));
        let mut delivered = Vec::new();
        let sent = mailer
            .deliver_with(&db, "2026-10-19", &mut |mail| {
                delivered.push(mail.to_address.clone());
                Ok(())
            })
            .unwrap();
        // Layer 3: the daily cap (2 here) holds the third for tomorrow.
        assert_eq!(sent, 2);
        assert_eq!(delivered, ["owner@blog.test", "owner@blog.test"]);
        let tomorrow = mailer
            .deliver_with(&db, "2026-10-20", &mut |_| Ok(()))
            .unwrap();
        assert_eq!(tomorrow, 1, "nothing is dropped");
    }
}
