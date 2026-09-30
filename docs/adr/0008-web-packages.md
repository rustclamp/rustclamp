# ADR 0008: Self-contained web packages

Status: Accepted, 2026-09-29. Web-only first version.

## Context

Laravel packages carry their own routes, views, config and tests, and a service
provider registers them in the host app. RustClamp apps need the same unit of
reuse: a feature in its own crate that an app adds without copying code into
`app/`.

## Decision

- `rustclamp::web::Package` has one method, `routes(self, Router) -> Router`.
  It takes `self` by value so a package moves its settings into `'static`
  handlers.
- `Router::package(p)` adds a package as a `group`, so middleware the package
  declares wraps only its own routes. The app's global middleware still wraps
  them. Routes match in the order they are added.
- The app opts in with one explicit `.package(...)` line. Cargo has no package
  auto-discovery that doesn't rely on build scripts or linker tricks, so the
  dependency plus that line is the registration.
- Views and static files are compiled into the package with `include_str!`.
  `package_view(package, name, embedded, data)` serves the app's built override
  `public/build/views/vendor/{package}/{name}.html` when one exists. The app's
  Vite glob builds `app/resources/views/vendor/**` like any other view.
- Settings come from the app's `Config`, which is passed to the package's
  constructor (`From<&Config>` in the scaffold). There is no separate
  package config file to publish.
- `clamp init NAME --package` scaffolds a library crate with a view, a
  stylesheet, settings and tests that run on a bare `Router`, without an app.

## Deferred

These wait until a package needs them: a `vendor:publish` command (copying a
view by hand is enough for now), and package migrations, workers, scheduled
jobs and CLI commands. Those need `Package` hooks beyond `routes`; add each one
when a real package needs it.
