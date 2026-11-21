# Cloudflare deployment

This directory contains a Worker front door for the Rust HTTP MCP server.

Cloudflare runs the server in a **Cloudflare Container**, not as a pure
`wasm32-unknown-unknown` Worker. The server needs TCP/TLS, a persistent Tokio
runtime and bundled SQLite; rewriting the flight tools for a V8 isolate would
duplicate the Rust implementation and lose the local history store.

The Worker starts the container, probes `/health`, and forwards `/mcp` and
`/health` to it.

## Deploy

Prerequisites:

- a Cloudflare account with Containers enabled;
- Node.js and `npx`;
- Docker (or a Docker-compatible CLI) running locally, because Wrangler builds
  the `linux/amd64` image from the repository root `Dockerfile`;
- `npx wrangler login`.

From the repository root:

```bash
npx wrangler secret put HKIA_MCP_API_KEY
npx wrangler deploy
```

`HKIA_MCP_API_KEY` is optional. When set, `/mcp` requires
`Authorization: Bearer <key>` or `X-API-Key: <key>`; `/health` stays public.
Without it the endpoint is unauthenticated, so put Cloudflare Access or OAuth
in front of a public deployment if you need per-user identity.

The endpoint is:

```text
https://<worker-name>.<account>.workers.dev/mcp
```

## Transport

Cloudflare's current remote MCP transport is **Streamable HTTP** at `/mcp`.
Legacy SSE is not exposed by the Worker. The Rust server still supports SSE
when run directly.

## Pure WASM Worker status

A pure Rust Cloudflare Worker is not included. It would require replacing
`reqwest`/rustls, Tokio's multi-thread runtime, SQLite and the MCP transport
with Workers-specific APIs. If a true V8-isolate Worker becomes a hard
requirement, build a separate Worker crate with a `fetch` adapter rather than
changing this container.
