mod dashboard;
mod fusion;
mod sensor;
mod types;
mod world;

use chrono::Utc;
use fusion::FusionEngine;
use types::Track;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{interval, Duration};
use tracing::{info, warn};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let objects = world::spawn_world();
    info!("Sentinel starting with {} simulated ground-truth objects", objects.len());
    let (tx, mut rx) = mpsc::channel(256);
    tokio::spawn(sensor::run_radar_sensor(objects.clone(), tx.clone()));
    tokio::spawn(sensor::run_camera_sensor(objects.clone(), tx.clone()));
    let gps_handle = tokio::spawn(sensor::run_gps_sensor(objects.clone(), tx.clone()));
    drop(tx);

    // Day 8 fault injection: after 15s of normal operation, kill the
    // GPS sensor task outright (simulating hardware failure or jamming)
    // while radar and camera keep running. This exercises graceful
    // degradation under partial sensor loss without tearing down the
    // whole pipeline — existing tracks should keep updating fine via
    // the two remaining sensors, and the system shouldn't crash or
    // hang just because one input source disappeared.
    info!("Fault injection armed: GPS sensor will be killed after 15s");
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(15)).await;
        gps_handle.abort();
        warn!("FAULT INJECTED: GPS sensor task aborted; radar and camera continue");
    });

    // Broadcast channel: every sweep tick pushes the current set of
    // tracks out to any connected dashboard clients. Capacity is small
    // since subscribers are expected to keep up with a ~10Hz feed; a
    // lagging client skips ahead (see dashboard.rs) rather than the
    // tracker itself being slowed down by a slow consumer.
    // Broadcast channel now uses Arc<Vec<Track>> to prevent N deep clones per client.
    let (snapshot_tx, _) = broadcast::channel::<Arc<Vec<Track>>>(16);
    let dashboard_addr: SocketAddr = "127.0.0.1:3000".parse().unwrap();
    tokio::spawn(dashboard::run_dashboard_server(
        snapshot_tx.clone(),
        dashboard_addr,
    ));

    let mut engine = FusionEngine::new();
    // Lifecycle transitions (Coasting, Dropped) are driven by wall-clock
    // time, not by incoming readings, so they need their own tick
    // independent of rx.recv() — a track with no sensor coverage still
    // has to age out even while nothing arrives on the channel.
    let mut sweep_interval = interval(Duration::from_millis(100));
    let mut reading_count: u64 = 0;

    loop {
        tokio::select! {
            maybe_reading = rx.recv() => {
                match maybe_reading {
                    Some(reading) => {
                        reading_count += 1;
                        engine.process_reading(reading);
                        if reading_count % 20 == 0 {
                            log_track_summary(&engine, reading_count);
                        }
                    }
                    None => {
                        info!(
                            "All sensors disconnected; {} tracks remained at shutdown",
                            engine.track_count()
                        );
                        break;
                    }
                }
            }
            _ = sweep_interval.tick() => {
                engine.sweep(Utc::now());
                // Ignore send errors: "no dashboard client connected yet"
                // is the normal case, not a failure condition.
                // Deep-clone the tracks exactly once, move them into immutable Arc slice.
                // Every connected dashboard client receives a pointer to this single allocation.
                let snapshot: Arc<Vec<Track>> = engine.tracks().cloned().collect::<Vec<_>>().into();
                let _ = snapshot_tx.send(snapshot);
            }
        }
    }
}

fn log_track_summary(engine: &FusionEngine, reading_count: u64) {
    info!(
        "--- fusion snapshot after {} readings: {} active tracks ---",
        reading_count,
        engine.track_count()
    );
    for track in engine.tracks() {
        info!(
            "  [{:?}] {} pos=({:.1}, {:.1}) vel=({:.1}, {:.1}) conf={:.2} sensors={:?}",
            track.status,
            track.id,
            track.position.x,
            track.position.y,
            track.velocity.0,
            track.velocity.1,
            track.confidence,
            track.contributing_sensors
        );
    }
}
