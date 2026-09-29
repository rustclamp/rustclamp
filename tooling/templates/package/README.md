# __NAME__

A RustClamp package, created with `clamp init --package`. It is self-contained:
its routes, middleware, views, static files, config and tests live here, and
`cargo test` runs them without an app.

```
src/lib.rs                  the package: `impl Package`, its routes and settings
resources/views/index.html  views, compiled in with include_str!
resources/css/              static files, served by the package's own routes
tests/routes.rs             tests against a bare Router
```

## Use it in an app

Add the dependency to the app's `Cargo.toml`:

```toml
__NAME__ = { path = "../__NAME__" }
```

Then add it to the app's routes in `app/lib.rs`:

```rust
let router = Router::new().package(__CRATE__::__STRUCT__::from(config));
```

Middleware the package declares wraps only its own routes. Routes match in the
order they are added, so app routes declared before `.package(...)` win.

## Settings

`__ENV___TITLE` in the app's `.env` sets the page title.

## Override a view

Copy `resources/views/index.html` to the app's
`app/resources/views/vendor/__NAME__/index.html`. The app's Vite build picks it
up, and `package_view` serves it instead of the package's copy. `<!--title-->`
markers still get filled.
