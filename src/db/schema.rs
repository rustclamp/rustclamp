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
        let columns: Vec<String> = table.columns.iter().map(Column::sql).collect();
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
    indexes: Vec<String>,
}

impl Table {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            columns: Vec::new(),
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
            references: None,
        });
        self.columns.last_mut().expect("just pushed")
    }

    /// `id INTEGER PRIMARY KEY`: SQLite's row id, assigned on insert.
    pub fn id(&mut self) {
        self.column("id", "INTEGER PRIMARY KEY");
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
    references: Option<String>,
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

    /// A foreign key to `table`'s `id`; deleting that row deletes this one.
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
        if let Some(table) = &self.references {
            sql.push_str(&format!(" REFERENCES {table} (id) ON DELETE CASCADE"));
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
    fn schema_sql_runs_in_sqlite() {
        let db = Db::open(&Config::parse("DB_DATABASE=:memory:"));
        let migrations = [
            Up(
                "0001",
                Schema::create("posts", |table| {
                    table.id();
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
            sql.execute("INSERT INTO posts (slug) VALUES ('a')", [])?;
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
