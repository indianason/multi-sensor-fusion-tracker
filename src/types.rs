use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

impl Position {
    pub fn distance_to(&self, other: &Position) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SensorKind {
    Radar,
    Camera,
    Gps,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorReading {
    pub sensor_id: Uuid,
    pub sensor_kind: SensorKind,
    pub contact_hint: String,
    pub position: Position,
    pub timestamp: DateTime<Utc>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackStatus {
    Tentative,
    Confirmed,
    Coasting,
    Dropped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: Uuid,
    pub position: Position,
    pub velocity: (f64, f64),
    pub status: TrackStatus,
    pub confidence: f64,
    pub last_updated: DateTime<Utc>,
    pub contributing_sensors: Vec<SensorKind>,
    pub history: Vec<Position>,
}