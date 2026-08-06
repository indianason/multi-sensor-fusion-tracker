use crate::types::{Position, SensorKind, SensorReading};
use crate::world::{sim_time, SimObject};
use chrono::Utc;
use rand::Rng;
use tokio::sync::mpsc::Sender;
use tokio::time::{sleep, Duration};
use uuid::Uuid;

fn gaussian_noise(std_dev: f64) -> f64 {
    let mut rng = rand::thread_rng();
    let u1: f64 = rng.gen_range(1e-9..1.0);
    let u2: f64 = rng.gen_range(0.0..1.0);
    let z0 = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
    z0 * std_dev
}

pub async fn run_radar_sensor(objects: Vec<SimObject>, tx: Sender<SensorReading>) {
    let sensor_id = Uuid::new_v4();
    loop {
        let t = sim_time();
        for obj in &objects {
            let truth = obj.true_position_at(t);
            let range = (truth.x.powi(2) + truth.y.powi(2)).sqrt();
            let bearing = truth.y.atan2(truth.x);
            let noisy_range = range + gaussian_noise(2.0);
            let noisy_bearing = bearing + gaussian_noise(0.02);
            let noisy_pos = Position {
                x: noisy_range * noisy_bearing.cos(),
                y: noisy_range * noisy_bearing.sin(),
            };
            let reading = SensorReading {
                sensor_id, sensor_kind: SensorKind::Radar,
                contact_hint: obj.label.to_string(), position: noisy_pos,
                timestamp: Utc::now(), confidence: 0.8,
            };
            if tx.send(reading).await.is_err() { return; }
        }
        sleep(Duration::from_millis(100)).await;
    }
}

pub async fn run_camera_sensor(objects: Vec<SimObject>, tx: Sender<SensorReading>) {
    let sensor_id = Uuid::new_v4();
    loop {
        let t = sim_time();
        for obj in &objects {
            let truth = obj.true_position_at(t);
            let dist_from_origin = (truth.x.powi(2) + truth.y.powi(2)).sqrt();
            let dropout_chance = (dist_from_origin / 2000.0).min(0.4);
            if rand::thread_rng().gen_bool(dropout_chance) { continue; }
            let noise_std = 3.0 + dist_from_origin * 0.01;
            let noisy_pos = Position {
                x: truth.x + gaussian_noise(noise_std),
                y: truth.y + gaussian_noise(noise_std),
            };
            let reading = SensorReading {
                sensor_id, sensor_kind: SensorKind::Camera,
                contact_hint: obj.label.to_string(), position: noisy_pos,
                timestamp: Utc::now(), confidence: (1.0 - dropout_chance).max(0.3),
            };
            if tx.send(reading).await.is_err() { return; }
        }
        sleep(Duration::from_millis(200)).await;
    }
}

pub async fn run_gps_sensor(objects: Vec<SimObject>, tx: Sender<SensorReading>) {
    let sensor_id = Uuid::new_v4();
    loop {
        let t = sim_time();
        for obj in &objects {
            let truth = obj.true_position_at(t);
            let noisy_pos = Position {
                x: truth.x + gaussian_noise(1.5),
                y: truth.y + gaussian_noise(1.5),
            };
            let reading = SensorReading {
                sensor_id, sensor_kind: SensorKind::Gps,
                contact_hint: obj.label.to_string(), position: noisy_pos,
                timestamp: Utc::now(), confidence: 0.95,
            };
            if tx.send(reading).await.is_err() { return; }
        }
        sleep(Duration::from_millis(1000)).await;
    }
}