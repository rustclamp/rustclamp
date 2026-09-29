# Vue Todo starter

A frontend-only Vue 3 + TypeScript todo app using the Vite+ toolchain. Tasks,
filters, add, complete, delete, progress, and localStorage persistence are
already wired so you can change the UI or behavior immediately.

## Run it

With Node.js and npm installed, run these from this directory:

```sh
npm install
npm run dev
```

Open the local URL printed by Vite+. `npm run build` creates a production
build, and `npm run check` runs Vite+'s project checks. The kit pins the
project-local Vite+ CLI, so a separate global `vp` installation is optional.

## Try changing it

- Add due dates or priorities to `Task` in `src/App.vue`.
- Split tasks into pages or lists.
- Replace localStorage with a RustClamp HTTP API.
- Use the visual layout as a starting point for a RustClamp landing page or blog.

This is a browser-only demo: it has no server, accounts, or sync between
browsers. The RustClamp landing page and documentation remain in their current
sites; this starter does not copy their production content.
