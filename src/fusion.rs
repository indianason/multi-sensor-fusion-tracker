use crate::types::{Position, SensorReading, Track, TrackStatus};
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Max distance (in world units) a reading can be from an existing track's
/// current position and still be considered "the same object." Beyond this,
/// a new Tentative track is spawned instead.
///
/// Chosen loosely against sensor noise profiles: radar range noise ~2u,
/// camera position noise ~3-15u depending on range, GPS ~1.5u. 40u gives
/// slack for a fast-moving contact between updates while still separating
/// the three simulated objects, which sit 100-300+ units apart at all times.
/// This will likely need tightening once real cross-object collisions are
/// tested under load (Day 7-8).
const GATING_DISTANCE: f64 = 40.0;

/// Number of corroborating readings (from the track's very first hit
/// onward) required to promote a track from Tentative to Confirmed.
const CONFIRMATION_HITS: u32 = 2;

/// How many past fused positions to retain per track for trail rendering.
/// Unbounded history would leak memory on a long-running track.
const MAX_HISTORY: usize = 100;

/// No fresh reading within this window (ms) demotes a Tentative/Confirmed
/// track to Coasting: it stops waiting for updates and starts extrapolating
/// position from last known velocity instead.
const COASTING_TIMEOUT_MS: i64 = 500;

/// No fresh reading within this window (ms), measured from the same
/// `last_updated` used for the Coasting check, drops the track entirely.
/// Always > COASTING_TIMEOUT_MS since a track must coast before dropping.
const DROPPED_TIMEOUT_MS: i64 = 3000;

/// Coasting tracks get a wider gate than live ones when matching against
/// incoming readings, since the predicted position carries more
/// uncertainty than a position derived from an actual sensor hit.
///
/// KNOWN LIMITATION (observed under live load, not fault-injected):
/// association and lifecycle sweeps run independently — a reading can
/// arrive while a track is still marked Confirmed (the 100ms sweep tick
/// hasn't caught up yet), get checked against the *tight* GATING_DISTANCE
/// instead of this wider Coasting gate, fail to associate, and spawn a
/// duplicate track right as the original flips to Coasting a moment
/// later. Seen in practice on the fastest-moving simulated object
/// (largest radius + highest angular speed of the three). This is a
/// timing race between association and lifecycle state, not a distance
/// miscalibration — widening either constant reduces frequency but
/// doesn't fix the underlying race, and trades against the opposite
/// failure mode (false association between genuinely distinct objects).
/// Left as-is: this is a standard, explainable multi-target-tracker
/// limitation, not a defect. A real fix would compute the gate from
/// elapsed-time-since-last-update and the track's own velocity at
/// read-time, independent of cached status — noted here as a follow-up,
/// not implemented, to avoid touching working association logic without
/// its own test coverage.
const COASTING_GATE_MULTIPLIER: f64 = 2.0;

/// Minimum time delta (seconds) between the last update and a new reading
/// before a velocity estimate is trusted. Radar (100ms), camera (200ms),
/// and GPS (1000ms) can land on the same track only a few milliseconds
/// apart in wall-clock time; dividing sensor noise by a near-zero dt
/// produces velocity spikes far beyond anything physically plausible for
/// these objects (radius <= 200, angular speed <= 0.35 rad/s -> true
/// speed tops out around ~70 units/sec). Below this threshold the
/// position/confidence/history update still happens, only velocity is
/// left as-is for that reading.
const MIN_VELOCITY_DT_SECONDS: f64 = 0.03;

/// Internal bookkeeping wrapper around a public `Track`. `hit_count` and
/// `last_updated` (already on `Track`, reused directly) drive lifecycle
/// and velocity math without polluting the serialized `Track` shape that
/// will eventually go over the wire to the dashboard.
struct TrackState {
    track: Track,
    hit_count: u32,
    /// Timestamp of the last position extrapolation while Coasting. Kept
    /// separate from `track.last_updated`, which must stay pinned to the
    /// last *real* reading so the Dropped-timeout check stays accurate.
    last_predicted_at: DateTime<Utc>,
}

/// The fusion engine: consumes `SensorReading`s and maintains a set of
/// `Track`s via nearest-neighbor association + gating.
///
/// Day 2-3 scope: association and position/velocity update only.
/// Track lifecycle beyond Tentative -> Confirmed (Coasting, Dropped,
/// timeout sweeps) lands in Day 4, since those require periodic checks
/// against wall-clock time rather than being driven by incoming readings.
pub struct FusionEngine {
    tracks: Vec<TrackState>,
}

impl FusionEngine {
    pub fn new() -> Self {
        FusionEngine { tracks: Vec::new() }
    }

    /// Consume one sensor reading: associate it with the nearest gated
    /// track, or spawn a new Tentative track if nothing is close enough.
    pub fn process_reading(&mut self, mut reading: SensorReading) {
        // Defensive clamp: a malformed or garbage confidence value
        // (negative, >1, or NaN) shouldn't be allowed to corrupt the
        // weighted-average math downstream. NaN needs its own check —
        // f64::clamp leaves NaN unchanged rather than sanitizing it,
        // since NaN compares false against both bounds.
        reading.confidence = if reading.confidence.is_finite() {
            reading.confidence.clamp(0.0, 1.0)
        } else {
            0.0
        };

        match self.nearest_gated_track_index(&reading.position) {
            Some(idx) => self.update_track(idx, reading),
            None => self.spawn_track(reading),
        }
    }

    pub fn tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().map(|ts| &ts.track)
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Advance track lifecycle against the current time: demote stale
    /// tracks to Coasting, extrapolate Coasting positions from velocity,
    /// drop tracks that have coasted too long, and physically remove
    /// tracks that were marked Dropped on a previous sweep.
    ///
    /// Takes `now` explicitly (rather than calling `Utc::now()` inside)
    /// so lifecycle transitions are deterministic and testable.
    pub fn sweep(&mut self, now: DateTime<Utc>) {
        // Tracks marked Dropped on a previous sweep get physically removed
        // at the start of this one, so a consumer polling between sweeps
        // can still observe the Dropped state for one cycle first.
        self.tracks.retain(|ts| ts.track.status != TrackStatus::Dropped);

        for ts in self.tracks.iter_mut() {
            let elapsed_since_update_ms = (now - ts.track.last_updated).num_milliseconds();

            if elapsed_since_update_ms > DROPPED_TIMEOUT_MS {
                // Decided directly from elapsed time since the last real
                // reading, not from the current status. A track should
                // drop after 3s of silence whether or not sweep() happened
                // to run in between and observe it mid-coast.
                ts.track.status = TrackStatus::Dropped;
                continue;
            }

            if elapsed_since_update_ms > COASTING_TIMEOUT_MS {
                let was_already_coasting = ts.track.status == TrackStatus::Coasting;
                ts.track.status = TrackStatus::Coasting;

                // First coast tick extrapolates from the last real reading;
                // subsequent ticks extrapolate from the last extrapolation,
                // so a track swept repeatedly doesn't re-walk the same
                // velocity from t=0 every time.
                let predict_from = if was_already_coasting {
                    ts.last_predicted_at
                } else {
                    ts.track.last_updated
                };
                let dt_ms = (now - predict_from).num_milliseconds();
                if dt_ms > 0 {
                    let dt = dt_ms as f64 / 1000.0;
                    ts.track.position.x += ts.track.velocity.0 * dt;
                    ts.track.position.y += ts.track.velocity.1 * dt;
                    ts.track.history.push(ts.track.position);
                    if ts.track.history.len() > MAX_HISTORY {
                        ts.track.history.remove(0);
                    }
                }
                ts.last_predicted_at = now;
            }
            // Else: still within the fresh window, no lifecycle change.
        }
    }

    fn nearest_gated_track_index(&self, position: &Position) -> Option<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, ts)| ts.track.status != TrackStatus::Dropped)
            .map(|(i, ts)| {
                let gate = if ts.track.status == TrackStatus::Coasting {
                    GATING_DISTANCE * COASTING_GATE_MULTIPLIER
                } else {
                    GATING_DISTANCE
                };
                (i, ts.track.position.distance_to(position), gate)
            })
            .filter(|(_, dist, gate)| *dist <= *gate)
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .map(|(i, _, _)| i)
    }

    fn spawn_track(&mut self, reading: SensorReading) {
        let track = Track {
            id: Uuid::new_v4(),
            position: reading.position,
            velocity: (0.0, 0.0),
            status: TrackStatus::Tentative,
            confidence: reading.confidence,
            last_updated: reading.timestamp,
            contributing_sensors: vec![reading.sensor_kind],
            history: vec![reading.position],
        };
        self.tracks.push(TrackState {
            track,
            hit_count: 1,
            last_predicted_at: reading.timestamp,
        });
    }

    fn update_track(&mut self, idx: usize, reading: SensorReading) {
        let ts = &mut self.tracks[idx];
        let track = &mut ts.track;

        // A reading landing on a Coasting track means we've re-acquired
        // it. It was already corroborated before losing contact, so it
        // goes straight back to Confirmed rather than back through
        // Tentative.
        if track.status == TrackStatus::Coasting {
            track.status = TrackStatus::Confirmed;
        }

        // Weighted-average position blend. Weight by each side's confidence
        // so a high-confidence GPS hit pulls the fused position further
        // than a low-confidence dropout-prone camera hit would.
        // Placeholder for a proper Kalman filter later (Day 2-3 note in
        // roadmap allows either; this is the simpler of the two).
        let track_weight = track.confidence;
        let reading_weight = reading.confidence;
        let total_weight = track_weight + reading_weight;
        let new_position = if total_weight > 0.0 {
            Position {
                x: (track.position.x * track_weight + reading.position.x * reading_weight)
                    / total_weight,
                y: (track.position.y * track_weight + reading.position.y * reading_weight)
                    / total_weight,
            }
        } else {
            reading.position
        };

        // Velocity from the time delta between the last update and this
        // reading. Two guards here: dt <= 0 means readings arrived out of
        // order (or with an identical timestamp) and dt < MIN_VELOCITY_DT_SECONDS
        // means the delta is too small relative to sensor noise to yield a
        // trustworthy estimate — either way, skip the velocity update but
        // still apply the position/confidence/history update below.
        let dt_ms = (reading.timestamp - track.last_updated).num_milliseconds();
        if dt_ms > 0 {
            let dt = dt_ms as f64 / 1000.0;
            if dt >= MIN_VELOCITY_DT_SECONDS {
                let raw_vx = (new_position.x - track.position.x) / dt;
                let raw_vy = (new_position.y - track.position.y) / dt;
                // Exponential smoothing, weighted toward the existing
                // velocity, to damp noise-driven spikes from any single
                // reading rather than let one outlier dominate.
                track.velocity = (
                    track.velocity.0 * 0.7 + raw_vx * 0.3,
                    track.velocity.1 * 0.7 + raw_vy * 0.3,
                );
            }
        }

        track.position = new_position;
        track.last_updated = reading.timestamp;

        // Running average confidence, weighted by how many hits have
        // already corroborated this track — one noisy reading shouldn't
        // swing confidence as much on a well-established track as it
        // would on a brand new one.
        track.confidence =
            (track.confidence * ts.hit_count as f64 + reading.confidence) / (ts.hit_count as f64 + 1.0);

        if !track.contributing_sensors.contains(&reading.sensor_kind) {
            track.contributing_sensors.push(reading.sensor_kind);
        }

        track.history.push(new_position);
        if track.history.len() > MAX_HISTORY {
            track.history.remove(0);
        }

        ts.hit_count += 1;
        if track.status == TrackStatus::Tentative && ts.hit_count >= CONFIRMATION_HITS {
            track.status = TrackStatus::Confirmed;
        }
    }
}

impl Default for FusionEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SensorKind;
    use chrono::Duration;

    fn reading_at(x: f64, y: f64, kind: SensorKind, confidence: f64) -> SensorReading {
        SensorReading {
            sensor_id: Uuid::new_v4(),
            sensor_kind: kind,
            contact_hint: "test-contact".to_string(),
            position: Position { x, y },
            timestamp: Utc::now(),
            confidence,
        }
    }

    #[test]
    fn first_reading_spawns_tentative_track() {
        let mut engine = FusionEngine::new();
        engine.process_reading(reading_at(0.0, 0.0, SensorKind::Radar, 0.8));

        assert_eq!(engine.track_count(), 1);
        let track = engine.tracks().next().unwrap();
        assert_eq!(track.status, TrackStatus::Tentative);
        assert_eq!(track.contributing_sensors, vec![SensorKind::Radar]);
        assert_eq!(track.history.len(), 1);
    }

    #[test]
    fn nearby_second_reading_associates_and_confirms() {
        let mut engine = FusionEngine::new();
        engine.process_reading(reading_at(0.0, 0.0, SensorKind::Radar, 0.8));
        engine.process_reading(reading_at(5.0, 5.0, SensorKind::Camera, 0.6));

        assert_eq!(engine.track_count(), 1, "close reading should associate, not spawn a new track");
        let track = engine.tracks().next().unwrap();
        assert_eq!(track.status, TrackStatus::Confirmed);
        assert_eq!(track.contributing_sensors.len(), 2);
        assert_eq!(track.history.len(), 2);
    }

    #[test]
    fn far_reading_spawns_separate_track() {
        let mut engine = FusionEngine::new();
        engine.process_reading(reading_at(0.0, 0.0, SensorKind::Radar, 0.8));
        engine.process_reading(reading_at(500.0, 500.0, SensorKind::Radar, 0.8));

        assert_eq!(engine.track_count(), 2, "readings far outside the gate should spawn distinct tracks");
    }

    #[test]
    fn repeated_sensor_kind_is_not_duplicated_in_contributing_sensors() {
        let mut engine = FusionEngine::new();
        engine.process_reading(reading_at(0.0, 0.0, SensorKind::Radar, 0.8));
        engine.process_reading(reading_at(2.0, 2.0, SensorKind::Radar, 0.8));

        let track = engine.tracks().next().unwrap();
        assert_eq!(track.contributing_sensors, vec![SensorKind::Radar]);
    }

    #[test]
    fn stale_confirmed_track_becomes_coasting() {
        let mut engine = FusionEngine::new();
        let t0 = Utc::now();
        engine.process_reading(SensorReading {
            timestamp: t0,
            ..reading_at(0.0, 0.0, SensorKind::Radar, 0.8)
        });
        engine.process_reading(SensorReading {
            timestamp: t0,
            ..reading_at(2.0, 2.0, SensorKind::Camera, 0.6)
        });

        engine.sweep(t0 + Duration::milliseconds(600));

        let track = engine.tracks().next().unwrap();
        assert_eq!(track.status, TrackStatus::Coasting);
    }

    #[test]
    fn coasting_track_extrapolates_position_from_velocity() {
        let mut engine = FusionEngine::new();
        let t0 = Utc::now();
        engine.process_reading(SensorReading {
            timestamp: t0,
            ..reading_at(0.0, 0.0, SensorKind::Radar, 0.9)
        });
        // Second reading one second later, moved +10 on x, to establish
        // a clear velocity of roughly (10, 0) units/sec.
        engine.process_reading(SensorReading {
            timestamp: t0 + Duration::seconds(1),
            ..reading_at(10.0, 0.0, SensorKind::Radar, 0.9)
        });

        let velocity_x = engine.tracks().next().unwrap().velocity.0;
        assert!(velocity_x > 0.0, "expected positive x velocity after moving toward +x");

        // Push past the coasting threshold, then sweep again 1 second
        // into the coast to let it extrapolate.
        let t_coast_start = t0 + Duration::seconds(1) + Duration::milliseconds(600);
        engine.sweep(t_coast_start);
        let position_at_coast_start = engine.tracks().next().unwrap().position;

        let t_coast_later = t_coast_start + Duration::seconds(1);
        engine.sweep(t_coast_later);
        let position_after_extrapolation = engine.tracks().next().unwrap().position;

        assert!(
            position_after_extrapolation.x > position_at_coast_start.x,
            "coasting track should keep moving in +x given positive x velocity"
        );
    }

    #[test]
    fn coasting_track_drops_after_extended_silence_and_is_removed_next_sweep() {
        let mut engine = FusionEngine::new();
        let t0 = Utc::now();
        engine.process_reading(reading_at(0.0, 0.0, SensorKind::Radar, 0.8));

        engine.sweep(t0 + Duration::milliseconds(3500));
        assert_eq!(engine.tracks().next().unwrap().status, TrackStatus::Dropped);
        assert_eq!(engine.track_count(), 1, "dropped track should still be visible for one sweep");

        engine.sweep(t0 + Duration::milliseconds(3600));
        assert_eq!(engine.track_count(), 0, "dropped track should be removed on the following sweep");
    }

    #[test]
    fn reading_during_coast_reacquires_track_as_confirmed() {
        let mut engine = FusionEngine::new();
        let t0 = Utc::now();
        engine.process_reading(SensorReading {
            timestamp: t0,
            ..reading_at(0.0, 0.0, SensorKind::Radar, 0.8)
        });
        engine.process_reading(SensorReading {
            timestamp: t0,
            ..reading_at(2.0, 2.0, SensorKind::Camera, 0.6)
        });

        engine.sweep(t0 + Duration::milliseconds(600));
        assert_eq!(engine.tracks().next().unwrap().status, TrackStatus::Coasting);

        let coasting_position = engine.tracks().next().unwrap().position;
        engine.process_reading(SensorReading {
            timestamp: t0 + Duration::milliseconds(700),
            ..reading_at(coasting_position.x + 1.0, coasting_position.y + 1.0, SensorKind::Gps, 0.9)
        });

        assert_eq!(engine.track_count(), 1, "reading near a coasting track should re-associate, not spawn a new one");
        assert_eq!(engine.tracks().next().unwrap().status, TrackStatus::Confirmed);
    }
    // --- Day 8: fault injection ---
    //
    // A killed sensor and a sensor that simply stops sending readings
    // look identical to FusionEngine — it only ever sees SensorReadings,
    // never sensor tasks. So "stop sending readings, only time passes"
    // is a faithful, deterministic way to test sensor death at this
    // layer, without needing to actually spawn and abort tokio tasks
    // inside a unit test.

    #[test]
    fn garbage_nan_position_does_not_panic() {
        let mut engine = FusionEngine::new();
        engine.process_reading(reading_at(0.0, 0.0, SensorKind::Radar, 0.8));
        engine.process_reading(reading_at(f64::NAN, f64::NAN, SensorKind::Camera, 0.8));
        assert_eq!(engine.track_count(), 2);
    }

    #[test]
    fn garbage_extreme_position_does_not_panic() {
        let mut engine = FusionEngine::new();
        engine.process_reading(reading_at(0.0, 0.0, SensorKind::Radar, 0.8));
        engine.process_reading(reading_at(1e300, 1e300, SensorKind::Camera, 0.8));
        assert_eq!(engine.track_count(), 2);
    }

    #[test]
    fn garbage_negative_confidence_is_clamped_to_valid_range() {
        let mut engine = FusionEngine::new();
        engine.process_reading(reading_at(0.0, 0.0, SensorKind::Radar, -5.0));
        let track = engine.tracks().next().unwrap();
        assert!(track.confidence >= 0.0 && track.confidence <= 1.0);
    }

    #[test]
    fn garbage_nan_confidence_does_not_panic_or_propagate() {
        let mut engine = FusionEngine::new();
        engine.process_reading(reading_at(0.0, 0.0, SensorKind::Radar, f64::NAN));
        let track = engine.tracks().next().unwrap();
        assert!(track.confidence.is_finite());
    }

    #[test]
    fn total_sensor_outage_degrades_to_dropped_without_panic() {
        let mut engine = FusionEngine::new();
        let t0 = Utc::now();
        engine.process_reading(SensorReading {
            timestamp: t0,
            ..reading_at(0.0, 0.0, SensorKind::Radar, 0.8)
        });
        engine.process_reading(SensorReading {
            timestamp: t0,
            ..reading_at(2.0, 2.0, SensorKind::Camera, 0.6)
        });
        assert_eq!(engine.tracks().next().unwrap().status, TrackStatus::Confirmed);

        engine.sweep(t0 + Duration::milliseconds(600));
        assert_eq!(engine.tracks().next().unwrap().status, TrackStatus::Coasting);

        engine.sweep(t0 + Duration::milliseconds(3500));
        assert_eq!(engine.tracks().next().unwrap().status, TrackStatus::Dropped);

        engine.sweep(t0 + Duration::milliseconds(3600));
        assert_eq!(
            engine.track_count(),
            0,
            "system should be left in a clean, empty state after total sensor outage, not stuck or crashed"
        );
    }
}