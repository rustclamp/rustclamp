# Scheduler

This runnable example composes job declarations through Kernel's target
composition API, advances a controlled Core `Clock` through regular and missed
intervals, and demonstrates stopping admission while an active job drains. It
uses a standard-library executor, not Tokio, cron, or an async task scheduler.

```sh
cargo run --offline --locked --manifest-path rustclamp/examples/04-scheduler/Cargo.toml --example 04-scheduler
```

## What can I do now?

- [Messaging](../05-messaging/README.md): send messages between processes
- [Device loop](../07-device-loop/README.md): drive a deterministic loop instead of a clock
