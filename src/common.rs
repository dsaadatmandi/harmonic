use chrono::prelude::Utc;
use crate::harmonic::HealthStatus;

pub struct HealthStatus {
    time: i64,
    state: HealthStatus,
}

impl HealthStatus {
    pub fn new(state: Status) -> Self {
        HealthStatus {
            time: Utc::now().timestamp_micros(),
            state,
        }
    }
}