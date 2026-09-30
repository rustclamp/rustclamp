//! Migration SQL from a table description, like Laravel's `Schema`.
//!
//! Names are written into the SQL as given; they come from the app's code,
//! never from a request. SQLite is the only dialect so far.

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
    /// assert!(sql.starts_with("CREATE TABLE comments (\n    id INTEGER PRIMARY KEY,"));
    /// assert!(sql.contains("post_id INTEGER NOT NULL REFERENCES posts (id)"));
    /// assert!(sql.contains("email TEXT UNIQUE"));
    /// assert!(sql.ends_with("CREATE INDEX comments_post_id_index ON comments (post_id);"));
    /// ```
    pub fn create(name: &str, define: impl FnOnce(&mut Table)) -> String {
        let mut table = Table::new(name);
        define(&mut table);
        let mut columns: Vec<String> = table.columns.iter().map(Column::sql).collect();
        columns.extend(table.constraints.iter().cloned());
        let mut sql = format!("CREATE TABLE {name} (\n    {}\n);", columns.join(",\n    "));
        for index in &table.indexes {
            sql.push('\n');
            sql.push_str(index);
        }
        sql
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
    /// assert_eq!(sql, "ALTER TABLE posts ADD COLUMN published INTEGER NOT NULL DEFAULT 0;");
    /// ```
    pub fn table(name: &str, define: impl FnOnce(&mut Table)) -> String {
        let mut table = Table::new(name);
        define(&mut table);
        assert!(
            table.constraints.is_empty(),
            "ALTER TABLE cannot add a primary key; use Schema::create"
        );
        let mut statements: Vec<String> = table
            .columns
            .iter()
            .map(|column| format!("ALTER TABLE {name} ADD COLUMN {};", column.sql()))
            .collect();
        statements.extend(table.indexes);
        statements.join("\n")
    }

    /// `DROP TABLE`.
    pub fn drop(name: &str) -> String {
        format!("DROP TABLE {name};")
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

    fn column(&mut self, name: &str, kind: &str) -> &mut Column {
        self.columns.push(Column {
            name: name.to_owned(),
            kind: kind.to_owned(),
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
        self.column("id", "INTEGER PRIMARY KEY");
    }

    /// `id INTEGER PRIMARY KEY AUTOINCREMENT`: like [`id`](Self::id), but SQLite
    /// never reuses the id of a deleted row.
    pub fn auto_id(&mut self) {
        self.column("id", "INTEGER PRIMARY KEY AUTOINCREMENT");
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
        self.column("public_id", "TEXT").unique()
    }

    /// A text column, for short values such as names.
    pub fn string(&mut self, name: &str) -> &mut Column {
        self.column(name, "TEXT")
    }

    /// A text column, for long values such as bodies. SQLite stores it like
    /// [`string`](Self::string); the name says what it holds.
    pub fn text(&mut self, name: &str) -> &mut Column {
        self.column(name, "TEXT")
    }

    /// A 64-bit integer column.
    pub fn integer(&mut self, name: &str) -> &mut Column {
        self.column(name, "INTEGER")
    }

    /// A floating-point column.
    pub fn real(&mut self, name: &str) -> &mut Column {
        self.column(name, "REAL")
    }

    /// A `0`/`1` integer column; rusqlite reads it as `bool`.
    pub fn boolean(&mut self, name: &str) -> &mut Column {
        self.column(name, "INTEGER")
    }

    /// A UTC `YYYY-MM-DD HH:MM:SS` text column.
    pub fn timestamp(&mut self, name: &str) -> &mut Column {
        self.column(name, "TEXT")
    }

    /// `created_at` and `updated_at`, both defaulting to the insert time.
    pub fn timestamps(&mut self) {
        self.timestamp("created_at").default("CURRENT_TIMESTAMP");
        self.timestamp("updated_at").default("CURRENT_TIMESTAMP");
    }

    /// An integer column for another table's `id`; add
    /// [`references`](Column::references) for the foreign key.
    pub fn foreign_id(&mut self, name: &str) -> &mut Column {
        self.column(name, "INTEGER")
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
    kind: String,
    nullable: bool,
    unique: bool,
    default: Option<String>,
    check: Option<String>,
    references: Option<String>,
    on_delete: OnDelete,
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
    /// or `"CURRENT_TIMESTAMP"`.
    pub fn default(&mut self, sql: &str) -> &mut Self {
        self.default = Some(sql.to_owned());
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

    fn sql(&self) -> String {
        let mut sql = format!("{} {}", self.name, self.kind);
        if !self.nullable && !self.kind.contains("PRIMARY KEY") {
            sql.push_str(" NOT NULL");
        }
        if self.unique {
            sql.push_str(" UNIQUE");
        }
        if let Some(default) = &self.default {
            sql.push_str(&format!(" DEFAULT {default}"));
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
    use crate::config::Config;
    use crate::db::{Db, Migration};

    struct Up(&'static str, String);

    impl Migration for Up {
        fn name(&self) -> &'static str {
            self.0
        }
        fn up(&self) -> String {
            self.1.clone()
        }
        fn down(&self) -> String {
            String::new()
        }
    }

    #[test]
    fn constraints_are_enforced_by_sqlite() {
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
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
        let run = |sql: &str| db.with(|c| c.execute(sql, []));
        assert!(run("INSERT INTO users (age) VALUES (-1)").is_err(), "CHECK");
        run("INSERT INTO users (age) VALUES (30)").unwrap();
        run("INSERT INTO users (age) VALUES (31)").unwrap();
        run("DELETE FROM users WHERE id = 2").unwrap();
        run("INSERT INTO users (age) VALUES (32)").unwrap();
        let id: i64 = db
            .with(|c| c.query_row("SELECT max(id) FROM users", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(id, 3, "AUTOINCREMENT does not reuse ids");
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
            .with(|c| c.query_row("SELECT user_id FROM notes", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(orphaned, None, "SET NULL");
        let memberships: i64 = db
            .with(|c| c.query_row("SELECT count(*) FROM memberships", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(memberships, 0, "CASCADE stays the default");
    }

    #[test]
    #[should_panic(expected = "ALTER TABLE cannot add a primary key")]
    fn alter_rejects_primary_key() {
        Schema::table("t", |table| table.primary_key(&["a"]));
    }

    #[test]
    fn schema_sql_runs_in_sqlite() {
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
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
                    table.boolean("published").default("0");
                    table.index(&["published"]);
                }),
            ),
        ];
        let all: Vec<&dyn Migration> = migrations.iter().map(|m| m as &dyn Migration).collect();
        db.migrate(&all).unwrap();
        db.with(|sql| {
            sql.execute(
                "INSERT INTO posts (public_id, slug) VALUES (?1, 'a')",
                [crate::uuid::Uuid::v7()],
            )?;
            let stored: crate::uuid::Uuid =
                sql.query_row("SELECT public_id FROM posts", [], |row| row.get(0))?;
            assert_eq!(stored.version(), 7);
            sql.execute("INSERT INTO comments (post_id, body) VALUES (1, 'hi')", [])?;
            sql.execute("DELETE FROM posts", [])
        })
        .unwrap();
        let comments: i64 = db
            .with(|sql| sql.query_row("SELECT count(*) FROM comments", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(comments, 0, "deleting a post deletes its comments");
        let orphan =
            db.with(|sql| sql.execute("INSERT INTO comments (post_id, body) VALUES (9, 'x')", []));
        assert!(orphan.is_err(), "foreign keys are enforced");
        db.migrate(&[&Up("0004", Schema::drop("comments"))])
            .unwrap();
    }
}
