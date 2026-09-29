// Lists every file in `app/database/migrations/` and `app/database/seeders/`
// for `database::migrations()` and `database::seeders()`: adding a file there
// is all it takes.
fn main() {
    rustclamp::build::migrations("app/database/migrations");
    rustclamp::build::seeders("app/database/seeders");
}
