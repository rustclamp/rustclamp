# Scheduler

This runnable example composes a job declaration through Kernel's target
composition API, then advances a controlled Core `Clock` through two ticks and
a multi-interval misfire. It uses a standard-library executor, not Tokio, cron,
or an async task scheduler.

```sh
cargo run --offline --locked --manifest-path rustclamp/examples/10-scheduler/Cargo.toml --example 10-scheduler
```
