# ADR 0028: MQTT transport for the worker service

Status: Accepted, 2026-10-07.

## Context

Stage 7 (Integrations) lists MQTT. `WorkerService` (ADR 0020) runs handlers over any `Transport`, and the only shipped one is `SqliteQueue` (ADR 0026), which a single process owns. Devices and other services publish to MQTT brokers, so a worker should be able to consume a topic directly. MQTT has QoS 1 acks but no nack, no per-message headers in 3.1.1, and no broker-side retry or dead-letter queue, so the service's settlements need a mapping.

## Decision

1. **`rustclamp_worker::mqtt`, behind an optional feature `mqtt`** (implies `service`). It uses `rumqttc` 0.25 with default features off: async on the Tokio the `service` feature already pulls, and 14 more packages. Writing MQTT framing, keep-alive and reconnects on std would be far more code than the adapter.
2. **`MqttTransport::connect(options, filter, dead_topic)`** forces manual acks and a persistent session (`clean_session = false`), subscribes to `filter` at QoS 1, and drives the event loop on a spawned task that buffers incoming publishes. The buffer is bounded by the broker's QoS 1 in-flight limit, since nothing is acked until it settles. Connection errors are retried every second and are not returned from `claim`. `WorkerService::run` returns on a claim error without draining, so the first broker restart would stop the worker.
3. **Settlements:**
   - `claim` returns buffered publishes. A payload that is not a JSON `MessageEnvelope` is `Claim::Malformed`.
   - `Done` sends the PUBACK.
   - `DeadLetter` republishes the original payload to `dead_topic` at QoS 1, then acks. The reason and error aren't carried, because MQTT 3.1.1 has no headers. They are in `ServiceEvent::DeadLettered`.
   - `Release` (shutdown) sends no ack, so the broker redelivers the message to the same client id on the next connect.
   - **Retry** stays in-process in the service, as for every transport. There is no `Settlement::Retry` (ADR 0026, open question 1), and MQTT has no nack to hand a message back early.
   - `recover` is the default no-op, because the broker's session already redelivers unacked messages.
4. **`mqtt::publish(client, topic, message)`** sends an envelope as JSON at QoS 1, for producers and an outbox relay. It returns once the publish is queued on the client, not when the broker has it.
5. **No TLS.** rumqttc's `use-rustls` feature pulls a second rustls stack with native certs and a TLS provider. An app that needs TLS can turn that feature on in its own manifest and pass `MqttOptions` with a TLS transport, because `connect` takes the options as given. MQTT 5, WebSockets and shared subscriptions are also left out until an app needs them.

## Consequences

- Delivery is at least once. A reconnect while a handler runs redelivers that message, and acks still queued when the transport drops are lost, so those messages come back as well. Handlers must be idempotent, as with every transport.
- Acks go out in settle order, not arrival order. Mosquitto accepts that. Brokers that need in-order PUBACKs (rumqttd) redeliver out-of-order acked messages after a reconnect, so an app on such a broker runs with `concurrency: 1` until the adapter reorders acks.
- The persistent session ties redelivery to the client id. Two workers with one id would steal each other's sessions. For several consumers, give each its own id and topic, or wait for MQTT 5 shared subscriptions.
- To the service, a broker that stays down looks like an empty queue. The transport reports the outage itself: a warning on stderr at the first connection error, again every minute while the broker stays down, and once on reconnect. `MqttTransport::outage()` returns how long the outage has lasted, for health checks. The warnings go to stderr, not the app's log channel (worker#14).
- `tools/boundaries.py` allows the rumqttc closure for `rustclamp-worker` and lists `rumqttc` in `OPTIONAL`.
- Tests: a live test in `worker/tests/mqtt.rs` covers claim and ack, dead-lettering a malformed payload, and redelivery of an unacked message after a reconnect. It is skipped unless `RUSTCLAMP_TEST_MQTT_URL` is set. CI's `examples` job runs `eclipse-mosquitto:2` with its bundled anonymous-listener config and sets the variable. An embedded broker (`rumqttd`) as a dev-dependency was rejected, because the allowlist covers dev-dependencies and rumqttd pulls an HTTP stack, metrics and config parsing.
