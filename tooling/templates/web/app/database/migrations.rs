//! Every migration, oldest first. Each is a struct in its own file under
//! `migrations/` implementing `Migration`: `up` changes the schema, `down`
//! undoes it. They run at startup; `cargo run -- migrate:rollback` undoes the
//! last batch. Never edit one that has run anywhere, add a new one instead.
//!
//! A new one, `migrations/create_posts.rs`:
//!
//! ```ignore
//! use rustclamp::db::{Migration, Schema};
//!
//! pub struct CreatePosts;
//!
//! impl Migration for CreatePosts {
//!     fn name(&self) -> &'static str {
//!         "2026_09_29_000001_create_posts"
//!     }
//!     fn up(&self) -> String {
//!         Schema::create("posts", |table| {
//!             table.id();
//!             table.string("title");
//!             table.text("body");
//!             table.timestamps();
//!         })
//!     }
//!     fn down(&self) -> String {
//!         Schema::drop("posts")
//!     }
//! }
//! ```
//!
//! then `mod create_posts;` here and `&create_posts::CreatePosts` in `all()`.

use rustclamp::db::Migration;

pub fn all() -> Vec<&'static dyn Migration> {
    vec![]
}
