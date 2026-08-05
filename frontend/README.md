# Smiðr web frontend

Vite + Svelte 5 + TypeScript UI for the `smidr` server. Talks to the
Rust backend over `GET/POST/DELETE /api/projects` and the `/api/session`
WebSocket protocol defined in `src/lib/ws.ts`.

## Build (reproducible)

```sh
npm install
npm run build
```

This produces `frontend/dist/`. The Rust binary embeds that directory via
`rust-embed` when built with:

```sh
cargo build --features embed-frontend
```

If `frontend/dist` is missing when that feature is compiled, the Rust build
fails with a message telling you to run `npm install && npm run build` here
first. Run the two commands above before building the embedded binary, and
re-run `npm run build` any time frontend source changes.

## Development

Run the Vite dev server, which proxies `/api` (including WebSocket upgrades)
to a locally running backend:

```sh
npm run dev
```

In a separate terminal, start the backend on the port the proxy expects:

```sh
smidr --port 8787
```

Then open the URL Vite prints (typically http://localhost:5173).

## Type checking

```sh
npm run check
```

Runs `svelte-check` over the project with `--fail-on-warnings`.
