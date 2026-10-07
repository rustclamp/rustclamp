# ADR 0027: Redis

Status: Accepted, 2026-10-07.

## Context

Stage 7 (Integrations) lists Redis. A `rustclamp` app keeps its rate limits in process (`web::Throttle`), so two instances behind one load balancer each allow the full limit, and a restart forgets every count. The app's other state is in SQLite, so Redis isn't needed as a database or a session store. What's missing is shared, short-lived counters and keys.

## Decision

- **An optional `redis` feature** (`config` only) with `rustclamp::redis::Redis`: a synchronous RESP2 client over std `TcpStream`. No new dependency, like the SMTP client in ADR 0015. `REDIS_URL` uses the usual `redis://[[user]:password@]host[:port][/db]` form, with percent-encoded credentials. `AUTH` and `SELECT` run on connect.
- **One connection behind a mutex**, shared by clones, like `Db`'s writer. It connects lazily. After a network or protocol error it drops the connection, and the next call reconnects. The failed command is **not retried**, because Redis may already have run it (an `INCR` would count twice). After a failed connect, calls fail fast for one second, so a Redis that is down doesn't cost every request a 5 s connect timeout.
- **Commands:** `command` for any command, plus `get`, `set` (optional TTL), `del` and `hit`. `hit` is a fixed-window counter: one `EVAL` does `INCR`, sets `PEXPIRE` on the first hit, and returns `PTTL`. It is atomic, so a key never ends up counting without an expiry.
- **`Throttle::shared(redis, prefix)`** counts hits in Redis, so all instances share one limit. While Redis can't be reached, each instance falls back to its own in-process count: limits stay in force, just per instance.
- **Not included:** TLS (`rediss://` is refused with a clear error; reach a remote Redis over a private network or a tunnel), RESP3, pub/sub, cluster and sentinel, and a connection pool. Each waits until an app needs it; the pool also waits for measurements, as with #39.

## Consequences

Tests use a fake RESP server on a local `TcpListener`. CI's `test` job also runs a `redis:8-alpine` service, and `RUSTCLAMP_TEST_REDIS_URL` turns on a live test there. Commands from all threads go through one connection, so a slow command (`KEYS *`) blocks every other caller.
