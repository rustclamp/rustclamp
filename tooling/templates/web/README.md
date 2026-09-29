# Clamp web

Created with `clamp init --web`: a Rust server with a Vite+ and Tailwind 4 frontend.

```sh
clamp dev
```

`clamp dev` runs `npm install` the first time, then `Procfile.dev`: Vite+
rebuilds `public/build/` on every save and the Rust server serves it. Open the
address the server prints (the first free port from 8080, or `PORT` if set) and
refresh after changes.

```
app/routes.rs              routes: GET / is the welcome view, GET /api/health returns JSON
app/main.rs, app/lib.rs    start the server; lib.rs exposes routes() to tests
resources/views/           pages (Vite entries), e.g. welcome.html
resources/css, resources/js
public/                    web root for static files; public/build/ is generated
tests/                     cargo test
```

The server itself (ports, requests, static files) is `rustclamp::web`, the
framework's `web` feature. `cargo dev` starts only the Rust server; run
`npm run build` first so `public/build/` exists.
