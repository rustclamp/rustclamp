//! Migration SQL from a table description, like Laravel's `Schema`.
//!
//! Names are written into the SQL as given; they come from the app's code,
//! never from a request. Each call renders SQLite and Postgres SQL (ADR 0034);
//! [`Db::migrate`](super::Db::migrate) runs the one for its engine.

use super::{Dialect, Sql};

/// Builds the SQL for one migration.
pub struct Schema;

impl Schema {
    /// `CREATE TABLE` with the columns `define` adds, then any indexes.
    ///
    /// ```
    /// use rustclamp::db::Schema;
    ///
    /// let sql = Schema::create("comments", |table| {
    ///     table.id();
    ///     table.foreign_id("post_id").references("posts");
    ///     table.text("body");
    ///     table.string("email").nullable().unique();
    ///     table.index(&["post_id"]);
    /// });
    /// let sql = sql.to_string(); // the SQLite rendering
    /// assert!(sql.starts_with("CREATE TABLE comments (\n    id INTEGER PRIMARY KEY,"));
    /// assert!(sql.contains("post_id INTEGER NOT NULL REFERENCES posts (id)"));
    /// assert!(sql.contains("email TEXT UNIQUE"));
    /// assert!(sql.ends_with("CREATE INDEX comments_post_id_index ON comments (post_id);"));
    /// ```
    pub fn create(name: &str, define: impl FnOnce(&mut Table)) -> Sql {
        let mut table = Table::new(name);
        define(&mut table);
        Sql::both(
            table.create_sql(Dialect::Sqlite),
            table.create_sql(Dialect::Postgres),
        )
    }

    /// `ALTER TABLE ... ADD COLUMN` for each column `define` adds, then any
    /// indexes. SQLite needs a default for a new column that is not nullable.
    ///
    /// ```
    /// use rustclamp::db::Schema;
    ///
    /// let sql = Schema::table("posts", |table| {
    ///     table.boolean("published").default("0");
    /// });
    /// assert_eq!(sql.to_string(), "ALTER TABLE posts ADD COLUMN published INTEGER NOT NULL DEFAULT 0;");
    /// ```
    pub fn table(name: &str, define: impl FnOnce(&mut Table)) -> Sql {
        let mut table = Table::new(name);
        define(&mut table);
        assert!(
            table.constraints.is_empty(),
            "ALTER TABLE cannot add a primary key; use Schema::create"
        );
        Sql::both(
            table.alter_sql(Dialect::Sqlite),
            table.alter_sql(Dialect::Postgres),
        )
    }

    /// `DROP TABLE`.
    pub fn drop(name: &str) -> Sql {
        format!("DROP TABLE {name};").into()
    }
}

/// The columns and indexes of one table, as [`Schema`] collects them.
pub struct Table {
    name: String,
    columns: Vec<Column>,
    constraints: Vec<String>,
    indexes: Vec<String>,
}

impl Table {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            columns: Vec::new(),
            constraints: Vec::new(),
            indexes: Vec::new(),
        }
    }

    fn create_sql(&self, dialect: Dialect) -> String {
        let name = &self.name;
        let mut columns: Vec<String> = self.columns.iter().map(|c| c.sql(dialect)).collect();
        columns.extend(self.constraints.iter().cloned());
        let mut sql = format!("CREATE TABLE {name} (\n    {}\n);", columns.join(",\n    "));
        for index in &self.indexes {
            sql.push('\n');
            sql.push_str(index);
        }
        sql
    }

    fn alter_sql(&self, dialect: Dialect) -> String {
        let name = &self.name;
        let mut statements: Vec<String> = self
            .columns
            .iter()
            .map(|column| format!("ALTER TABLE {name} ADD COLUMN {};", column.sql(dialect)))
            .collect();
        statements.extend(self.indexes.iter().cloned());
        statements.join("\n")
    }

    fn column(&mut self, name: &str, kind: Kind) -> &mut Column {
        self.columns.push(Column {
            name: name.to_owned(),
            kind,
            nullable: false,
            unique: false,
            default: None,
            check: None,
            references: None,
            on_delete: OnDelete::Cascade,
        });
        self.columns.last_mut().expect("just pushed")
    }

    /// `id INTEGER PRIMARY KEY`: SQLite's row id, assigned on insert.
    pub fn id(&mut self) {
        self.column("id", Kind::Id);
    }

    /// `id INTEGER PRIMARY KEY AUTOINCREMENT`: like [`id`](Self::id), but SQLite
    /// never reuses the id of a deleted row.
    pub fn auto_id(&mut self) {
        self.column("id", Kind::AutoId);
    }

    /// A primary key over `columns` together, for tables such as a join table
    /// that have no `id`. Only in [`Schema::create`].
    pub fn primary_key(&mut self, columns: &[&str]) {
        self.constraints
            .push(format!("PRIMARY KEY ({})", columns.join(", ")));
    }

    /// `public_id`: a unique UUID, the only ID shown outside the app (URLs,
    /// API payloads, mail links) while `id` stays internal. Fill it with
    /// [`Uuid::v7`](crate::uuid::Uuid::v7) on insert.
    pub fn public_id(&mut self) -> &mut Column {
        self.column("public_id", Kind::Text).unique()
    }

    /// A text column, for short values such as names.
    pub fn string(&mut self, name: &str) -> &mut Column {
        self.column(name, Kind::Text)
    }

    /// A text column, for long values such as bodies. SQLite stores it like
    /// [`string`](Self::string); the name says what it holds.
    pub fn text(&mut self, name: &str) -> &mut Column {
        self.column(name, Kind::Text)
    }

    /// A 64-bit integer column.
    pub fn integer(&mut self, name: &str) -> &mut Column {
        self.column(name, Kind::Integer)
    }

    /// A floating-point column.
    pub fn real(&mut self, name: &str) -> &mut Column {
        self.column(name, Kind::Real)
    }

    /// A boolean column: `0`/`1` integers on SQLite, `BOOLEAN` on Postgres;
    /// `Row::get` reads either as `bool`.
    pub fn boolean(&mut self, name: &str) -> &mut Column {
        self.column(name, Kind::Bool)
    }

    /// A UTC `YYYY-MM-DD HH:MM:SS` text column.
    pub fn timestamp(&mut self, name: &str) -> &mut Column {
        self.column(name, Kind::Text)
    }

    /// A binary column.
    pub fn blob(&mut self, name: &str) -> &mut Column {
        self.column(name, Kind::Blob)
    }

    /// `created_at` and `updated_at`, both defaulting to the insert time.
    pub fn timestamps(&mut self) {
        self.timestamp("created_at").default_now();
        self.timestamp("updated_at").default_now();
    }

    /// An integer column for another table's `id`; add
    /// [`references`](Column::references) for the foreign key.
    pub fn foreign_id(&mut self, name: &str) -> &mut Column {
        self.column(name, Kind::Integer)
    }

    /// An index on `columns`, named `{table}_{columns}_index`.
    pub fn index(&mut self, columns: &[&str]) {
        self.add_index("INDEX", "index", columns);
    }

    /// A unique index on `columns` together, named `{table}_{columns}_unique`.
    /// For one column, [`Column::unique`] is shorter.
    pub fn unique(&mut self, columns: &[&str]) {
        self.add_index("UNIQUE INDEX", "unique", columns);
    }

    fn add_index(&mut self, kind: &str, suffix: &str, columns: &[&str]) {
        let table = &self.name;
        self.indexes.push(format!(
            "CREATE {kind} {table}_{}_{suffix} ON {table} ({});",
            columns.join("_"),
            columns.join(", ")
        ));
    }
}

/// One column, returned by the [`Table`] methods so modifiers can follow.
/// Columns are `NOT NULL` unless [`nullable`](Self::nullable).
pub struct Column {
    name: String,
    kind: Kind,
    nullable: bool,
    unique: bool,
    default: Option<Default>,
    check: Option<String>,
    references: Option<String>,
    on_delete: OnDelete,
}

/// A column's type, rendered per dialect.
#[derive(Clone, Copy)]
enum Kind {
    Id,
    AutoId,
    Text,
    Integer,
    Real,
    Bool,
    Blob,
}

impl Kind {
    fn sql(self, dialect: Dialect) -> &'static str {
        match (self, dialect) {
            (Self::Id, Dialect::Sqlite) => "INTEGER PRIMARY KEY",
            (Self::AutoId, Dialect::Sqlite) => "INTEGER PRIMARY KEY AUTOINCREMENT",
            (Self::Id | Self::AutoId, Dialect::Postgres) => {
                "BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY"
            }
            (Self::Text, _) => "TEXT",
            (Self::Integer | Self::Bool, Dialect::Sqlite) => "INTEGER",
            (Self::Integer, Dialect::Postgres) => "BIGINT",
            (Self::Real, Dialect::Sqlite) => "REAL",
            (Self::Real, Dialect::Postgres) => "DOUBLE PRECISION",
            (Self::Bool, Dialect::Postgres) => "BOOLEAN",
            (Self::Blob, Dialect::Sqlite) => "BLOB",
            (Self::Blob, Dialect::Postgres) => "BYTEA",
        }
    }

    fn is_primary_key(self) -> bool {
        matches!(self, Self::Id | Self::AutoId)
    }
}

/// A column default: SQL as written, or a value that renders per dialect.
enum Default {
    Raw(String),
    Bool(bool),
    Now,
}

impl Default {
    fn sql(&self, dialect: Dialect) -> &str {
        match (self, dialect) {
            (Self::Raw(sql), _) => sql,
            (Self::Bool(false), Dialect::Sqlite) => "0",
            (Self::Bool(true), Dialect::Sqlite) => "1",
            (Self::Bool(false), Dialect::Postgres) => "FALSE",
            (Self::Bool(true), Dialect::Postgres) => "TRUE",
            (Self::Now, Dialect::Sqlite) => "CURRENT_TIMESTAMP",
            (Self::Now, Dialect::Postgres) => {
                "to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')"
            }
        }
    }
}

/// What happens to a row when the row its foreign key points at is deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnDelete {
    /// Delete this row too.
    #[default]
    Cascade,
    /// Set the column to `NULL`; the column must be [`nullable`](Column::nullable).
    SetNull,
    /// Refuse the delete, at once.
    Restrict,
    /// Refuse the delete when the statement ends (SQLite's default).
    NoAction,
}

impl OnDelete {
    fn sql(self) -> &'static str {
        match self {
            Self::Cascade => "CASCADE",
            Self::SetNull => "SET NULL",
            Self::Restrict => "RESTRICT",
            Self::NoAction => "NO ACTION",
        }
    }
}

impl Column {
    /// Allows `NULL`.
    pub fn nullable(&mut self) -> &mut Self {
        self.nullable = true;
        self
    }

    /// No two rows share a value.
    pub fn unique(&mut self) -> &mut Self {
        self.unique = true;
        self
    }

    /// The value for rows that do not set one, as SQL: `"0"`, `"'draft'"`
    /// or `"CURRENT_TIMESTAMP"`. It runs as written on every engine; for
    /// booleans and the insert time, [`default_bool`](Self::default_bool) and
    /// [`default_now`](Self::default_now) render per dialect.
    pub fn default(&mut self, sql: &str) -> &mut Self {
        self.default = Some(Default::Raw(sql.to_owned()));
        self
    }

    /// A boolean default: `0`/`1` on SQLite, `FALSE`/`TRUE` on Postgres.
    pub fn default_bool(&mut self, value: bool) -> &mut Self {
        self.default = Some(Default::Bool(value));
        self
    }

    /// The insert time as a UTC `YYYY-MM-DD HH:MM:SS` string, for a
    /// [`timestamp`](Table::timestamp) column.
    pub fn default_now(&mut self) -> &mut Self {
        self.default = Some(Default::Now);
        self
    }

    /// Only rows where the SQL condition holds, such as `"score >= 0"`.
    pub fn check(&mut self, sql: &str) -> &mut Self {
        self.check = Some(sql.to_owned());
        self
    }

    /// What deleting the referenced row does to this one; the default is
    /// [`OnDelete::Cascade`]. Only meaningful after [`references`](Self::references).
    pub fn on_delete(&mut self, action: OnDelete) -> &mut Self {
        self.on_delete = action;
        self
    }

    /// A foreign key to `table`'s `id`; deleting that row deletes this one
    /// unless [`on_delete`](Self::on_delete) says otherwise.
    pub fn references(&mut self, table: &str) -> &mut Self {
        self.references = Some(table.to_owned());
        self
    }

    fn sql(&self, dialect: Dialect) -> String {
        let mut sql = format!("{} {}", self.name, self.kind.sql(dialect));
        if !self.nullable && !self.kind.is_primary_key() {
            sql.push_str(" NOT NULL");
        }
        if self.unique {
            sql.push_str(" UNIQUE");
        }
        if let Some(default) = &self.default {
            sql.push_str(&format!(" DEFAULT {}", default.sql(dialect)));
        }
        if let Some(check) = &self.check {
            sql.push_str(&format!(" CHECK ({check})"));
        }
        if let Some(table) = &self.references {
            sql.push_str(&format!(
                " REFERENCES {table} (id) ON DELETE {}",
                self.on_delete.sql()
            ));
        }
        sql
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testing::each_engine;
    use crate::db::{Db, Migration};

    struct Up(&'static str, Sql);

    impl Migration for Up {
        fn name(&self) -> &'static str {
            self.0
        }
        fn up(&self) -> Sql {
            self.1.clone()
        }
        fn down(&self) -> Sql {
            "".into()
        }
    }

    #[test]
    fn constraints_are_enforced() {
        each_engine(constraints_are_enforced_on);
    }

    fn constraints_are_enforced_on(db: &Db) {
        let create = |name, sql| Up(name, sql);
        let migrations = [
            create(
                "0001",
                Schema::create("users", |table| {
                    table.auto_id();
                    table.integer("age").check("age >= 0");
                }),
            ),
            create(
                "0002",
                Schema::create("memberships", |table| {
                    table.foreign_id("user_id").references("users");
                    table.integer("team");
                    table.primary_key(&["user_id", "team"]);
                }),
            ),
            create(
                "0003",
                Schema::create("notes", |table| {
                    table.id();
                    table
                        .foreign_id("user_id")
                        .nullable()
                        .references("users")
                        .on_delete(OnDelete::SetNull);
                }),
            ),
            create(
                "0004",
                Schema::create("logs", |table| {
                    table.id();
                    table
                        .foreign_id("user_id")
                        .references("users")
                        .on_delete(OnDelete::Restrict);
                }),
            ),
        ];
        let all: Vec<&dyn Migration> = migrations.iter().map(|m| m as &dyn Migration).collect();
        db.migrate(&all).unwrap();
        let run = |sql: &str| db.with(|c| c.execute(sql, &[]));
        run("INSERT INTO users (age) VALUES (30)").unwrap();
        run("INSERT INTO users (age) VALUES (31)").unwrap();
        run("DELETE FROM users WHERE id = 2").unwrap();
        run("INSERT INTO users (age) VALUES (32)").unwrap();
        let id: i64 = db
            .with(|c| c.query_row("SELECT max(id) FROM users", &[], |r| r.get(0)))
            .unwrap();
        assert_eq!(id, 3, "ids are not reused");
        run("INSERT INTO memberships VALUES (1, 7)").unwrap();
        assert!(
            run("INSERT INTO memberships VALUES (1, 7)").is_err(),
            "composite PK"
        );
        run("INSERT INTO memberships VALUES (1, 8)").unwrap();
        run("INSERT INTO notes (user_id) VALUES (1)").unwrap();
        run("INSERT INTO logs (user_id) VALUES (3)").unwrap();
        assert!(run("DELETE FROM users WHERE id = 3").is_err(), "RESTRICT");
        run("DELETE FROM users WHERE id = 1").unwrap();
        let orphaned: Option<i64> = db
            .with(|c| c.query_row("SELECT user_id FROM notes", &[], |r| r.get(0)))
            .unwrap();
        assert_eq!(orphaned, None, "SET NULL");
        let memberships: i64 = db
            .with(|c| c.query_row("SELECT count(*) FROM memberships", &[], |r| r.get(0)))
            .unwrap();
        assert_eq!(memberships, 0, "CASCADE stays the default");
        // Last: a refused insert still uses up a Postgres identity value.
        assert!(run("INSERT INTO users (age) VALUES (-1)").is_err(), "CHECK");
    }

    #[test]
    #[should_panic(expected = "ALTER TABLE cannot add a primary key")]
    fn alter_rejects_primary_key() {
        Schema::table("t", |table| table.primary_key(&["a"]));
    }

    #[test]
    fn schema_sql_runs() {
        each_engine(schema_sql_runs_on);
    }

    fn schema_sql_runs_on(db: &Db) {
        let migrations = [
            Up(
                "0001",
                Schema::create("posts", |table| {
                    table.id();
                    table.public_id();
                    table.string("slug").unique();
                    table.real("score").default("0");
                    table.timestamps();
                }),
            ),
            Up(
                "0002",
                Schema::create("comments", |table| {
                    table.id();
                    table.foreign_id("post_id").references("posts");
                    table.text("body");
                    table.string("email").nullable();
                    table.index(&["post_id", "id"]);
                    table.unique(&["post_id", "email"]);
                }),
            ),
            Up(
                "0003",
                Schema::table("posts", |table| {
                    table.boolean("published").default_bool(false);
                    table.index(&["published"]);
                }),
            ),
        ];
        let all: Vec<&dyn Migration> = migrations.iter().map(|m| m as &dyn Migration).collect();
        db.migrate(&all).unwrap();
        db.with(|sql| {
            sql.execute(
                "INSERT INTO posts (public_id, slug) VALUES (?1, 'a')",
                &[&crate::uuid::Uuid::v7()],
            )?;
            let stored: crate::uuid::Uuid =
                sql.query_row("SELECT public_id FROM posts", &[], |row| row.get(0))?;
            assert_eq!(stored.version(), 7);
            sql.execute("INSERT INTO comments (post_id, body) VALUES (1, 'hi')", &[])?;
            sql.execute("DELETE FROM posts", &[])
        })
        .unwrap();
        let comments: i64 = db
            .with(|sql| sql.query_row("SELECT count(*) FROM comments", &[], |row| row.get(0)))
            .unwrap();
        assert_eq!(comments, 0, "deleting a post deletes its comments");
        let orphan =
            db.with(|sql| sql.execute("INSERT INTO comments (post_id, body) VALUES (9, 'x')", &[]));
        assert!(orphan.is_err(), "foreign keys are enforced");
        db.migrate(&[&Up("0004", Schema::drop("comments"))])
            .unwrap();
    }

    fn render(sql: &Sql, dialect: Dialect) -> String {
        sql.render(dialect).to_owned()
    }

    #[test]
    fn create_renders_per_dialect() {
        let sql = Schema::create("posts", |table| {
            table.id();
            table.public_id();
            table.foreign_id("user_id").references("users");
            table.string("title");
            table.text("body").nullable();
            table.integer("views").default("0");
            table.real("score").default("0");
            table.boolean("published").default_bool(false);
            table.blob("cover").nullable();
            table.timestamps();
            table.index(&["user_id"]);
        });
        assert_eq!(
            render(&sql, Dialect::Sqlite),
            "CREATE TABLE posts (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE,
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    body TEXT,
    views INTEGER NOT NULL DEFAULT 0,
    score REAL NOT NULL DEFAULT 0,
    published INTEGER NOT NULL DEFAULT 0,
    cover BLOB,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX posts_user_id_index ON posts (user_id);"
        );
        assert_eq!(sql.to_string(), render(&sql, Dialect::Sqlite));
        assert_eq!(
            render(&sql, Dialect::Postgres),
            "CREATE TABLE posts (
    id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE,
    user_id BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    body TEXT,
    views BIGINT NOT NULL DEFAULT 0,
    score DOUBLE PRECISION NOT NULL DEFAULT 0,
    published BOOLEAN NOT NULL DEFAULT FALSE,
    cover BYTEA,
    created_at TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')
);
CREATE INDEX posts_user_id_index ON posts (user_id);"
        );
    }

    #[test]
    fn auto_id_and_composite_keys_per_dialect() {
        let sql = Schema::create("logs", |table| {
            table.auto_id();
            table.boolean("ok").default_bool(true);
        });
        assert_eq!(
            render(&sql, Dialect::Sqlite),
            "CREATE TABLE logs (\n    id INTEGER PRIMARY KEY AUTOINCREMENT,\n    ok INTEGER NOT NULL DEFAULT 1\n);"
        );
        assert_eq!(
            render(&sql, Dialect::Postgres),
            "CREATE TABLE logs (\n    id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,\n    ok BOOLEAN NOT NULL DEFAULT TRUE\n);"
        );
        let join = Schema::create("tag_post", |table| {
            table.foreign_id("post_id");
            table.foreign_id("tag_id");
            table.primary_key(&["post_id", "tag_id"]);
            table.unique(&["tag_id", "post_id"]);
        });
        assert_eq!(
            render(&join, Dialect::Postgres),
            "CREATE TABLE tag_post (
    post_id BIGINT NOT NULL,
    tag_id BIGINT NOT NULL,
    PRIMARY KEY (post_id, tag_id)
);
CREATE UNIQUE INDEX tag_post_tag_id_post_id_unique ON tag_post (tag_id, post_id);"
        );
    }

    #[test]
    fn alter_and_drop_per_dialect() {
        let sql = Schema::table("posts", |table| {
            table.boolean("published").default_bool(false);
            table.timestamp("seen_at").nullable();
            table.index(&["published"]);
        });
        assert_eq!(
            render(&sql, Dialect::Sqlite),
            "ALTER TABLE posts ADD COLUMN published INTEGER NOT NULL DEFAULT 0;
ALTER TABLE posts ADD COLUMN seen_at TEXT;
CREATE INDEX posts_published_index ON posts (published);"
        );
        assert_eq!(
            render(&sql, Dialect::Postgres),
            "ALTER TABLE posts ADD COLUMN published BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE posts ADD COLUMN seen_at TEXT;
CREATE INDEX posts_published_index ON posts (published);"
        );
        for dialect in [Dialect::Sqlite, Dialect::Postgres] {
            assert_eq!(Schema::drop("posts").render(dialect), "DROP TABLE posts;");
            assert_eq!(Sql::from("SELECT 1").render(dialect), "SELECT 1");
        }
    }

    #[test]
    fn default_now_and_bool_run() {
        each_engine(default_now_and_bool_run_on);
    }

    fn default_now_and_bool_run_on(db: &Db) {
        db.migrate(&[&Up(
            "0001",
            Schema::create("flags", |table| {
                table.id();
                table.boolean("active").default_bool(true);
                table.blob("data").nullable();
                table.timestamp("at").default_now();
            }),
        )])
        .unwrap();
        db.with(|sql| {
            sql.execute(
                "INSERT INTO flags (data) VALUES (?1)",
                crate::db::params![vec![1u8, 2]],
            )
        })
        .unwrap();
        let (on, data, at): (bool, Vec<u8>, String) = db
            .with(|sql| {
                sql.query_row("SELECT active, data, at FROM flags", &[], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
            })
            .unwrap();
        assert!(on);
        assert_eq!(data, [1, 2]);
        assert_eq!(at.len(), "2026-10-13 00:00:00".len());
    }
}
