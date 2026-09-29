# ADR 0012: View Templates

Status: Accepted, 2026-09-30.

## Context

PROJECT.md §29 and phase 6 kept a template engine out until the architecture
was proven. The `nejctest/blog` app then showed what `<!--key-->` slots cost:

- Lists were built as HTML with `format!` in controllers, each value passed
  through `escape` by hand. A forgotten `escape` would have been an XSS hole.
- The nav and footer were copied into all five views, differing only in which
  link is `aria-current`.
- Error views and form state used separate marker schemes (`<!--status-->`,
  `<!--old:field-->`).

## Decision

A small Blade subset in `rustclamp::web` (`web/view.rs`), std-only like the
rest of `web`:

- `{{ a.b }}` escapes, `{!! a.b !!}` does not. `@if(x)` / `@if(!x)` / `@else`,
  `@foreach(list as item)`, `@extends`, `@section` (block or inline value),
  `@yield`, `@include` (optionally with named values: `post: featured`,
  `title: 'Hi'`, `compact: true`), `{{-- --}}`, and `@@` / `@{{` as escapes.
- No expressions, filters or `@elseif`. The controller computes what the
  view shows.
- Data is `Value` (text, bool, list, map). Anything that implements `ToValue`
  becomes one, so models implement it with `Value::map(&[...])`. No serde,
  which the facade does not depend on (ADR 0003).
- Templates are rendered at request time from the HTML Vite built. A probe
  build confirmed that Vite passes every construct through unchanged, in text,
  attributes and asset attributes, and that it leaves layouts and partials
  without `<head>` untouched. Layouts stay Vite entries, since the hashed
  script and style tags are injected into them.
- An unknown top-level name, an unclosed block or a missing view is an error
  that names the view and line. It is logged, and the request answers `500`.
  A missing key of a map is empty text, as in Blade.
- The `asset` fallback no longer serves `public/build/views/`, so layouts and
  partials are never sent raw.

## Consequences

`render`, `Request::render` and `package_view` take `&[(&str, &dyn ToValue)]`.
The blog, the `--web` and `--package` templates, and the docs moved in the
same change. Templates are parsed on every request (a `ponytail:` note marks
where to cache them if profiling shows the cost). There is no compile-time
checking of views, because the views are Vite output that exists only at
runtime.
