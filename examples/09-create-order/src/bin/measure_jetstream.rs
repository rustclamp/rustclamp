//! Measures serial publish confirmations and checks JetStream stream limits.

use async_nats::jetstream::{self, stream};
use std::error::Error;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
    let jetstream = jetstream::new(async_nats::connect(url).await?);
    let run_id = std::process::id();
    let subject = format!("bench.phase7.{run_id}");
    let stream_name = format!("RUSTCLAMP_P7_BENCH_{run_id}");

    jetstream
        .get_or_create_stream(stream::Config {
            name: stream_name.clone(),
            subjects: vec![subject.clone()],
            max_messages: 10_000,
            max_bytes: 64 * 1024 * 1024,
            discard: stream::DiscardPolicy::New,
            ..Default::default()
        })
        .await?;
    let started = Instant::now();
    for sequence in 0..1_000_u32 {
        let acknowledgement = jetstream
            .publish(subject.clone(), sequence.to_be_bytes().to_vec().into())
            .await?;
        acknowledgement.await?;
    }
    let elapsed = started.elapsed();
    println!(
        "published=1000 elapsed_ms={} messages_per_second={:.0}",
        elapsed.as_millis(),
        1_000.0 / elapsed.as_secs_f64()
    );
    jetstream.delete_stream(&stream_name).await?;

    let limited_name = format!("RUSTCLAMP_P7_LIMIT_{run_id}");
    let limited_subject = format!("limit.phase7.{run_id}");
    jetstream
        .get_or_create_stream(stream::Config {
            name: limited_name.clone(),
            subjects: vec![limited_subject.clone()],
            max_messages: 2,
            max_bytes: 1024,
            discard: stream::DiscardPolicy::New,
            ..Default::default()
        })
        .await?;
    for _ in 0..2 {
        jetstream
            .publish(limited_subject.clone(), "bounded".into())
            .await?
            .await?;
    }
    let overflow = jetstream
        .publish(limited_subject.clone(), "overflow".into())
        .await?
        .await;
    println!("third_publish_rejected={}", overflow.is_err());
    jetstream.delete_stream(&limited_name).await?;
    Ok(())
}
