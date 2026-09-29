// Lists every file in `app/database/migrations/`, `seeders/` and `states/`
// for `database::migrations()`, `database::seeders()` and `database::states`:
// adding a file there is all it takes.
fn main() {
    rustclamp::build::migrations("app/database/migrations");
    rustclamp::build::seeders("app/database/seeders");
    rustclamp::build::states("app/database/states");
}
