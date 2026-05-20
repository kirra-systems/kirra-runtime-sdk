// src/metrics.rs

use std::sync::atomic::{AtomicU64, Ordering};

pub struct LockFreeMetricsAggregator {
    pub total_processed_frames: AtomicU64,
    pub envelope_clamping_events: AtomicU64,
    pub rate_limiting_events: AtomicU64,
    pub authentication_failures: AtomicU64,
    pub tracking_jitter_violations: AtomicU64,
    pub trust_score: AtomicU64,
    pub active_worker_threads: AtomicU64,
}

impl LockFreeMetricsAggregator {
    pub fn new() -> Self {
        Self {
            total_processed_frames: AtomicU64::new(0),
            envelope_clamping_events: AtomicU64::new(0),
            rate_limiting_events: AtomicU64::new(0),
            authentication_failures: AtomicU64::new(0),
            tracking_jitter_violations: AtomicU64::new(0),
            trust_score: AtomicU64::new(100),
            active_worker_threads: AtomicU64::new(0),
        }
    }

    pub fn format_prometheus_metrics(&self, node_id: &str) -> String {
        let mut out = String::new();
        let write_metric = |buffer: &mut String, name: &str, mtype: &str, desc: &str, val: u64| {
            buffer.push_str(&format!("# HELP aegis_{} {}\n", name, desc));
            buffer.push_str(&format!("# TYPE aegis_{} {}\n", name, mtype));
            buffer.push_str(&format!("aegis_{}{{node_id=\"{}\"}} {}\n", name, node_id, val));
        };

        write_metric(&mut out, "processed_frames_total", "counter", "Total Modbus TCP write frames evaluated", self.total_processed_frames.load(Ordering::Relaxed));
        write_metric(&mut out, "envelope_clamping_events_total", "counter", "Total entries matching out-of-envelope parameters", self.envelope_clamping_events.load(Ordering::Relaxed));
        write_metric(&mut out, "rate_limiting_events_total", "counter", "Total entries triggering acceleration constraints", self.rate_limiting_events.load(Ordering::Relaxed));
        write_metric(&mut out, "authentication_failures_total", "counter", "Total failed administrative override sequences", self.authentication_failures.load(Ordering::Relaxed));
        write_metric(&mut out, "jitter_violations_total", "counter", "Total times runtime loops missed jitter margins", self.tracking_jitter_violations.load(Ordering::Relaxed));
        write_metric(&mut out, "trust_score", "gauge", "Active mathematical safety trust score boundary", self.trust_score.load(Ordering::Relaxed));
        write_metric(&mut out, "active_worker_threads", "gauge", "Concurrent thread saturation within worker pools", self.active_worker_threads.load(Ordering::Relaxed));

        out
    }
}
