use crate::types::Position;
use std::f64::consts::PI;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct SimObject {
    pub label: &'static str,
    pub center: Position,
    pub radius: f64,
    pub angular_speed: f64,
    pub phase: f64,
}

pub fn spawn_world() -> Vec<SimObject> {
    vec![
        SimObject { label: "contact-alpha", center: Position { x: 0.0, y: 0.0 }, radius: 200.0, angular_speed: 0.15, phase: 0.0 },
        SimObject { label: "contact-bravo", center: Position { x: 300.0, y: 150.0 }, radius: 120.0, angular_speed: -0.25, phase: 1.0 },
        SimObject { label: "contact-charlie", center: Position { x: -250.0, y: 100.0 }, radius: 180.0, angular_speed: 0.35, phase: 2.5 },
    ]
}

pub fn sim_time() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

impl SimObject {
    pub fn true_position_at(&self, t: f64) -> Position {
        let angle = self.phase + self.angular_speed * t;
        Position {
            x: self.center.x + self.radius * angle.cos(),
            y: self.center.y + self.radius * angle.sin(),
        }
    }

    pub fn true_velocity_at(&self, t: f64) -> (f64, f64) {
        let dt = 0.05;
        let p0 = self.true_position_at(t);
        let p1 = self.true_position_at(t + dt);
        ((p1.x - p0.x) / dt, (p1.y - p0.y) / dt)
    }
}

pub const TWO_PI: f64 = 2.0 * PI;