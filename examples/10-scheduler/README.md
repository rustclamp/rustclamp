# Scheduler

This runnable example composes job declarations through Kernel's target
composition API, advances a controlled Core `Clock` through regular and missed
intervals, and demonstrates stopping admission while an active job drains. It
uses a standard-library executor, not Tokio, cron, or an async task scheduler.

```sh
cargo run --offline --locked --manifest-path rustclamp/examples/10-scheduler/Cargo.toml --example 10-scheduler
```
