// src/metrics.rs

use std::sync::atomic::{AtomicU64, Ordering};

pub struct LockFreeMetricsAggregator {
    pub total_processed_frames: AtomicU64,
    pub envelope_clamping_events: AtomicU64,
    pub rate_limiting_events: AtomicU64,
    pub authentication_failures: AtomicU64,
    pub tracking_jitter_violations: AtomicU64,
}

impl LockFreeMetricsAggregator {
    pub fn new() -> Self {
        Self {
            total_processed_frames: AtomicU64::new(0),
            envelope_clamping_events: AtomicU64::new(0),
            rate_limiting_events: AtomicU64::new(0),
            authentication_failures: AtomicU64::new(0),
            tracking_jitter_violations: AtomicU64::new(0),
        }
    }

    pub fn format_prometheus_metrics(&self, node_id: &str) -> String {
        let mut out = String::new();
        let write_metric = |buffer: &mut String, name: &str, desc: &str, val: u64| {
            buffer.push_str(&format!("# HELP aegis_{} {}\n", name, desc));
            buffer.push_str(&format!("# TYPE aegis_{} counter\n", name));
            buffer.push_str(&format!("aegis_{}{{node_id=\"{}\"}} {}\n", name, node_id, val));
        };

        write_metric(&mut out, "processed_frames_total", "Total Modbus TCP write frames evaluated", self.total_processed_frames.load(Ordering::Relaxed));
        write_metric(&mut out, "envelope_clamping_events_total", "Total entries matching out-of-envelope parameters", self.envelope_clamping_events.load(Ordering::Relaxed));
        write_metric(&mut out, "rate_limiting_events_total", "Total entries triggering acceleration constraints", self.rate_limiting_events.load(Ordering::Relaxed));
        write_metric(&mut out, "authentication_failures_total", "Total failed administrative override sequences", self.authentication_failures.load(Ordering::Relaxed));
        write_metric(&mut out, "jitter_violations_total", "Total times runtime loops missed jitter margins", self.tracking_jitter_violations.load(Ordering::Relaxed));

        out
    }
}
