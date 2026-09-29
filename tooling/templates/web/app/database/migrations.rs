//! Every change to the database schema, oldest first. Each runs once, at
//! startup, and is recorded in the `migrations` table; never edit one that
//! has run anywhere, add a new one instead.

pub const ALL: &[(&str, &str)] = &[
    // ("0001_create_posts", "CREATE TABLE posts (
    //     id INTEGER PRIMARY KEY,
    //     title TEXT NOT NULL
    // )"),
];
