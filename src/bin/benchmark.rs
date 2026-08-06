use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Number of synthetic messages pushed through the channel per run.
/// Chosen high enough to produce stable throughput/latency numbers,
/// low enough that even the slowest polling variant finishes in a few
/// seconds on a laptop.
const MESSAGE_COUNT: usize = 50_000;

/// A single unit of synthetic work: just a timestamp, so we can measure
/// how long it sat in the channel before the consumer picked it up.
#[derive(Clone, Copy)]
struct Envelope {
    sent_at: Instant,
}

struct RunResult {
    total_duration: Duration,
    throughput_per_sec: f64,
    latencies_micros: Vec<u128>,
    empty_polls: u64,
}

#[tokio::main]
async fn main() {
    println!("=== Sentinel: event-driven vs naive-polling consumer benchmark ===");
    println!("Messages per run: {}\n", MESSAGE_COUNT);

    let event_driven = run_event_driven(MESSAGE_COUNT).await;
    let naive_fast_poll = run_naive_polling(MESSAGE_COUNT, Duration::from_micros(500)).await;
    let naive_slow_poll = run_naive_polling(MESSAGE_COUNT, Duration::from_millis(5)).await;

    print_report("Event-driven (channel.recv().await)", &event_driven);
    print_report("Naive polling, 500us interval", &naive_fast_poll);
    print_report("Naive polling, 5ms interval", &naive_slow_poll);

    println!("--- Summary ---");
    println!(
        "Event-driven:      {:>10.0} msg/sec, 0 wasted wakeups",
        event_driven.throughput_per_sec
    );
    println!(
        "Naive (fast poll):  {:>10.0} msg/sec, {} empty polls (CPU spent checking for nothing)",
        naive_fast_poll.throughput_per_sec, naive_fast_poll.empty_polls
    );
    println!(
        "Naive (slow poll):  {:>10.0} msg/sec, {} empty polls, but worse tail latency",
        naive_slow_poll.throughput_per_sec, naive_slow_poll.empty_polls
    );
}

/// Baseline: the exact pattern main.rs already uses. The task is
/// suspended until the runtime wakes it because data arrived — no
/// wasted checks, no fixed delay.
async fn run_event_driven(count: usize) -> RunResult {
    let (tx, mut rx) = mpsc::channel::<Envelope>(1024);

    let producer = tokio::spawn(async move {
        for _ in 0..count {
            let _ = tx
                .send(Envelope {
                    sent_at: Instant::now(),
                })
                .await;
        }
    });

    let start = Instant::now();
    let mut latencies = Vec::with_capacity(count);
    while let Some(envelope) = rx.recv().await {
        latencies.push(envelope.sent_at.elapsed().as_micros());
        if latencies.len() >= count {
            break;
        }
    }
    let total_duration = start.elapsed();
    producer.await.unwrap();

    RunResult {
        total_duration,
        throughput_per_sec: count as f64 / total_duration.as_secs_f64(),
        latencies_micros: latencies,
        empty_polls: 0,
    }
}

/// The naive alternative: check the channel on a fixed timer instead of
/// being woken on demand. `poll_interval` controls how long the
/// consumer sleeps after finding the channel empty — a tight interval
/// trades CPU cycles for lower latency, a loose interval trades latency
/// for fewer wasted checks. Both are real failure modes of a
/// hand-rolled polling loop; this benchmark shows both ends.
async fn run_naive_polling(count: usize, poll_interval: Duration) -> RunResult {
    let (tx, mut rx) = mpsc::channel::<Envelope>(1024);

    let producer = tokio::spawn(async move {
        for _ in 0..count {
            let _ = tx
                .send(Envelope {
                    sent_at: Instant::now(),
                })
                .await;
        }
    });

    let start = Instant::now();
    let mut latencies = Vec::with_capacity(count);
    let mut empty_polls: u64 = 0;
    let mut received = 0;

    while received < count {
        match rx.try_recv() {
            Ok(envelope) => {
                latencies.push(envelope.sent_at.elapsed().as_micros());
                received += 1;
            }
            Err(mpsc::error::TryRecvError::Empty) => {
                empty_polls += 1;
                tokio::time::sleep(poll_interval).await;
            }
            Err(mpsc::error::TryRecvError::Disconnected) => break,
        }
    }
    let total_duration = start.elapsed();
    producer.await.unwrap();

    RunResult {
        total_duration,
        throughput_per_sec: count as f64 / total_duration.as_secs_f64(),
        latencies_micros: latencies,
        empty_polls,
    }
}

fn print_report(label: &str, result: &RunResult) {
    let mut sorted = result.latencies_micros.clone();
    sorted.sort_unstable();
    let n = sorted.len();
    let percentile = |p: f64| -> u128 {
        if n == 0 {
            return 0;
        }
        let idx = ((p / 100.0) * (n as f64 - 1.0)).round() as usize;
        sorted[idx.min(n - 1)]
    };
    let mean: f64 = if n > 0 {
        sorted.iter().sum::<u128>() as f64 / n as f64
    } else {
        0.0
    };

    println!("--- {} ---", label);
    println!("  total time:   {:.3}s", result.total_duration.as_secs_f64());
    println!("  throughput:   {:.0} msg/sec", result.throughput_per_sec);
    println!("  latency mean: {:.1} us", mean);
    println!("  latency p50:  {} us", percentile(50.0));
    println!("  latency p95:  {} us", percentile(95.0));
    println!("  latency p99:  {} us", percentile(99.0));
    if result.empty_polls > 0 {
        println!("  empty polls:  {}", result.empty_polls);
    }
    println!();
}