// src/metrics.rs

use std::sync::atomic::{AtomicU64, Ordering};

use crate::verifier::FleetPosture;

/// WS-0.5 — why the posture-routing gate denied a request. Mirrors
/// `should_route_command`'s decision order exactly (see
/// `classify_gate_denial` in `gateway::policy_layer`), plus the HA
/// authority fence. A FIXED label set — Prometheus label cardinality
/// must stay bounded, so new deny causes get new variants here, never
/// free-form strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDenialReason {
    /// The request classified as `OperationalCommand::Unknown` (denied in
    /// every posture, before the cache is even consulted).
    UnknownCommand,
    /// Posture cache held `None` (cold start / reset) or its lock was
    /// poisoned — no posture evidence, fail closed.
    PostureCacheEmpty,
    /// Posture cache entry aged past its TTL — stale evidence, fail closed.
    PostureCacheStale,
    /// Fleet posture is `LockedOut` — everything is denied.
    LockedOut,
    /// Fleet posture is `Degraded` and the command is a write that is not
    /// the decel-gated `ActuatorMotion` deferral (ADR-0011 Option A).
    DegradedWriteDenied,
    /// The HA authority fence rejected a mutation: this instance's held
    /// epoch is stale (another instance promoted) or actuator authority
    /// could not be verified — self-demote + deny.
    HaFenced,
}

/// Escape a string for use inside a quoted Prometheus label value
/// (text exposition format 0.0.4): `\` → `\\`, `"` → `\"`, newline → `\n`.
fn escape_label_value(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            other => escaped.push(other),
        }
    }
    escaped
}

impl GateDenialReason {
    /// The stable Prometheus label value.
    pub fn as_label(self) -> &'static str {
        match self {
            Self::UnknownCommand      => "unknown_command",
            Self::PostureCacheEmpty   => "posture_cache_empty",
            Self::PostureCacheStale   => "posture_cache_stale",
            Self::LockedOut           => "locked_out",
            Self::DegradedWriteDenied => "degraded_write_denied",
            Self::HaFenced            => "ha_fenced",
        }
    }
}

/// WS-0.5 — point-in-time gauge values the `/metrics` handler reads from
/// live service state at scrape time (they are not accumulated counters).
/// Constructed by the binary's handler; passed to
/// [`FleetSafetyMetrics::format_prometheus`].
#[derive(Debug, Clone, Copy)]
pub struct FleetMetricsSnapshot {
    /// The EFFECTIVE routing posture (fail-closed: a cold / stale /
    /// poisoned cache reads as `LockedOut`, exactly as the gate treats it).
    pub effective_posture: FleetPosture,
    /// True when the effective posture is fail-closed synthetic (cold /
    /// stale / poisoned cache) rather than a live DAG verdict.
    pub posture_cache_stale: bool,
    /// The posture generation from the cache (0 when the cache is cold).
    pub posture_generation: u64,
    /// True when this instance is Active (accepting mutations), false for
    /// PassiveStandby.
    pub mode_active: bool,
    /// `AppState::audit_write_drops` — kinematic-deny audit records dropped
    /// on a Full/Closed audit-writer channel (A3). MUST be 0 when healthy.
    pub audit_write_drops: u64,
    /// `AppState::capture_drops` — learning-capture records dropped on a
    /// Full/Closed capture channel (non-safety).
    pub capture_drops: u64,
    /// `AppState::post_incident_write_failures` — post-incident forensic
    /// audit writes that could not be durably recorded (#104).
    pub post_incident_write_failures: u64,
    /// `AppState::command_source_write_failures` — command-source handoff
    /// audit writes that could not be durably recorded (#112).
    pub command_source_write_failures: u64,
}

/// WS-0.5 — the fleet-safety counter set behind `GET /metrics` on the
/// verifier binary. Lock-free atomics, incremented on the paths they
/// observe; formatted with [`format_prometheus`](Self::format_prometheus).
/// Lives on `AppState` so the posture engine, the routing gate, and the
/// HA promotion path can all reach it.
#[derive(Debug, Default)]
pub struct FleetSafetyMetrics {
    /// Committed posture TRANSITIONS by target posture (the periodic
    /// refresh traffic is not counted). Indexed: [Nominal, Degraded, LockedOut].
    transitions_nominal: AtomicU64,
    transitions_degraded: AtomicU64,
    transitions_locked_out: AtomicU64,
    /// Posture-routing gate denials (dropped commands) by reason.
    denials_unknown_command: AtomicU64,
    denials_posture_cache_empty: AtomicU64,
    denials_posture_cache_stale: AtomicU64,
    denials_locked_out: AtomicU64,
    denials_degraded_write: AtomicU64,
    denials_ha_fenced: AtomicU64,
    /// Completed standby→Active promotions (HA failover).
    ha_promotions: AtomicU64,
}

impl FleetSafetyMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count a COMMITTED posture transition (called by the posture engine
    /// after the audit-chain write succeeds — a suppressed transition is
    /// not counted, matching what subscribers observe).
    pub fn record_transition(&self, to: &FleetPosture) {
        let c = match to {
            FleetPosture::Nominal => &self.transitions_nominal,
            FleetPosture::Degraded => &self.transitions_degraded,
            FleetPosture::LockedOut => &self.transitions_locked_out,
        };
        c.fetch_add(1, Ordering::Relaxed);
    }

    /// Count a posture-routing gate denial (a dropped command).
    pub fn record_gate_denial(&self, reason: GateDenialReason) {
        let c = match reason {
            GateDenialReason::UnknownCommand      => &self.denials_unknown_command,
            GateDenialReason::PostureCacheEmpty   => &self.denials_posture_cache_empty,
            GateDenialReason::PostureCacheStale   => &self.denials_posture_cache_stale,
            GateDenialReason::LockedOut           => &self.denials_locked_out,
            GateDenialReason::DegradedWriteDenied => &self.denials_degraded_write,
            GateDenialReason::HaFenced            => &self.denials_ha_fenced,
        };
        c.fetch_add(1, Ordering::Relaxed);
    }

    /// Count a completed standby→Active promotion (HA failover).
    pub fn record_ha_promotion(&self) {
        self.ha_promotions.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn transition_count(&self, to: &FleetPosture) -> u64 {
        match to {
            FleetPosture::Nominal => &self.transitions_nominal,
            FleetPosture::Degraded => &self.transitions_degraded,
            FleetPosture::LockedOut => &self.transitions_locked_out,
        }
        .load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn ha_promotion_count(&self) -> u64 {
        self.ha_promotions.load(Ordering::Relaxed)
    }

    /// Render the Prometheus text exposition (format 0.0.4) for the fleet-
    /// safety series: the accumulated counters in `self` plus the
    /// scrape-time gauges in `snap`. Same `kirra_` prefix and style as
    /// `LockFreeMetricsAggregator::format_prometheus_metrics`.
    pub fn format_prometheus(&self, node_id: &str, snap: &FleetMetricsSnapshot) -> String {
        use std::fmt::Write as _;
        // node_id comes from env/hostname — escape it per the Prometheus
        // text-format rules for quoted label values (`\` → `\\`, `"` → `\"`,
        // newline → `\n`), or a hostile/odd instance id breaks every scrape.
        let node_id = escape_label_value(node_id);
        let node_id = node_id.as_str();
        let mut out = String::with_capacity(4096);

        // One HELP/TYPE block, then one sample line per label value.
        let family = |out: &mut String,
                          name: &str,
                          mtype: &str,
                          desc: &str,
                          samples: &[(&str, u64)]| {
            let _ = writeln!(out, "# HELP kirra_{name} {desc}");
            let _ = writeln!(out, "# TYPE kirra_{name} {mtype}");
            for (labels, val) in samples {
                if labels.is_empty() {
                    let _ = writeln!(out, "kirra_{name}{{node_id=\"{node_id}\"}} {val}");
                } else {
                    let _ = writeln!(
                        out,
                        "kirra_{name}{{node_id=\"{node_id}\",{labels}}} {val}"
                    );
                }
            }
        };

        // --- gauges (scrape-time state) ---
        family(
            &mut out,
            "fleet_posture",
            "gauge",
            "Effective fleet routing posture (0=Nominal 1=Degraded 2=LockedOut; \
             fail-closed: a cold/stale/poisoned posture cache reads as LockedOut)",
            &[("", match snap.effective_posture {
                FleetPosture::Nominal => 0,
                FleetPosture::Degraded => 1,
                FleetPosture::LockedOut => 2,
            })],
        );
        family(
            &mut out,
            "posture_cache_stale",
            "gauge",
            "1 when the effective posture is the fail-closed synthetic LockedOut \
             (cold/stale/poisoned posture cache) rather than a live DAG verdict",
            &[("", u64::from(snap.posture_cache_stale))],
        );
        family(
            &mut out,
            "posture_generation",
            "gauge",
            "Monotonic posture generation from the cache (0 when cold)",
            &[("", snap.posture_generation)],
        );
        family(
            &mut out,
            "mode_active",
            "gauge",
            "1 when this instance is Active (accepting mutations), 0 for PassiveStandby",
            &[("", u64::from(snap.mode_active))],
        );

        // --- counters (accumulated events) ---
        family(
            &mut out,
            "posture_transitions_total",
            "counter",
            "Committed fleet posture transitions by target posture \
             (periodic cache refreshes are not transitions)",
            &[
                ("posture=\"nominal\"", self.transitions_nominal.load(Ordering::Relaxed)),
                ("posture=\"degraded\"", self.transitions_degraded.load(Ordering::Relaxed)),
                ("posture=\"locked_out\"", self.transitions_locked_out.load(Ordering::Relaxed)),
            ],
        );
        family(
            &mut out,
            "gate_denials_total",
            "counter",
            "Commands dropped by the posture-routing gate (HTTP 503) by fail-closed reason",
            &[
                ("reason=\"unknown_command\"", self.denials_unknown_command.load(Ordering::Relaxed)),
                ("reason=\"posture_cache_empty\"", self.denials_posture_cache_empty.load(Ordering::Relaxed)),
                ("reason=\"posture_cache_stale\"", self.denials_posture_cache_stale.load(Ordering::Relaxed)),
                ("reason=\"locked_out\"", self.denials_locked_out.load(Ordering::Relaxed)),
                ("reason=\"degraded_write_denied\"", self.denials_degraded_write.load(Ordering::Relaxed)),
                ("reason=\"ha_fenced\"", self.denials_ha_fenced.load(Ordering::Relaxed)),
            ],
        );
        family(
            &mut out,
            "ha_promotions_total",
            "counter",
            "Completed standby-to-Active promotions (HA failover) performed by this instance",
            &[("", self.ha_promotions.load(Ordering::Relaxed))],
        );

        // --- drop / write-failure counters already accumulated on AppState ---
        family(
            &mut out,
            "audit_write_drops_total",
            "counter",
            "Kinematic-deny audit records dropped on a full/closed audit-writer \
             channel (A3) — MUST be 0 in a healthy deployment",
            &[("", snap.audit_write_drops)],
        );
        family(
            &mut out,
            "capture_drops_total",
            "counter",
            "Learning-capture records dropped on a full/closed capture channel (non-safety)",
            &[("", snap.capture_drops)],
        );
        family(
            &mut out,
            "post_incident_write_failures_total",
            "counter",
            "Post-incident forensic audit writes that could not be durably recorded (#104) \
             — MUST be 0 in a healthy deployment",
            &[("", snap.post_incident_write_failures)],
        );
        family(
            &mut out,
            "command_source_write_failures_total",
            "counter",
            "Command-source handoff audit writes that could not be durably recorded (#112) \
             — MUST be 0 in a healthy deployment",
            &[("", snap.command_source_write_failures)],
        );

        out
    }
}

pub struct LockFreeMetricsAggregator {
    pub total_processed_frames: AtomicU64,
    pub envelope_clamping_events: AtomicU64,
    pub rate_limiting_events: AtomicU64,
    pub authentication_failures: AtomicU64,
    pub tracking_jitter_violations: AtomicU64,
    pub trust_score: AtomicU64,
    pub active_worker_threads: AtomicU64,
}

impl Default for LockFreeMetricsAggregator {
    fn default() -> Self {
        Self::new()
    }
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
            buffer.push_str(&format!("# HELP kirra_{} {}\n", name, desc));
            buffer.push_str(&format!("# TYPE kirra_{} {}\n", name, mtype));
            buffer.push_str(&format!("kirra_{}{{node_id=\"{}\"}} {}\n", name, node_id, val));
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

// ---------------------------------------------------------------------------
// WS-0.5 tests — the fleet-safety exposition.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod fleet_metrics_tests {
    use super::*;

    fn snap(posture: FleetPosture, stale: bool) -> FleetMetricsSnapshot {
        FleetMetricsSnapshot {
            effective_posture: posture,
            posture_cache_stale: stale,
            posture_generation: 7,
            mode_active: true,
            audit_write_drops: 11,
            capture_drops: 12,
            post_incident_write_failures: 13,
            command_source_write_failures: 14,
        }
    }

    /// Every fleet-safety family is present with a HELP + TYPE block and a
    /// node_id label — the shape a Prometheus scraper parses.
    #[test]
    fn exposition_contains_every_family_with_help_and_type() {
        let m = FleetSafetyMetrics::new();
        let text = m.format_prometheus("node-a", &snap(FleetPosture::Nominal, false));
        for family in [
            "fleet_posture",
            "posture_cache_stale",
            "posture_generation",
            "mode_active",
            "posture_transitions_total",
            "gate_denials_total",
            "ha_promotions_total",
            "audit_write_drops_total",
            "capture_drops_total",
            "post_incident_write_failures_total",
            "command_source_write_failures_total",
        ] {
            assert!(
                text.contains(&format!("# HELP kirra_{family} ")),
                "missing HELP for kirra_{family}:\n{text}"
            );
            assert!(
                text.contains(&format!("# TYPE kirra_{family} ")),
                "missing TYPE for kirra_{family}:\n{text}"
            );
            assert!(
                text.contains(&format!("kirra_{family}{{node_id=\"node-a\"")),
                "missing sample line for kirra_{family}:\n{text}"
            );
        }
    }

    /// The posture gauge encodes 0/1/2 and the stale flag is independent.
    #[test]
    fn posture_gauge_encodes_the_fail_closed_mapping() {
        let m = FleetSafetyMetrics::new();
        for (posture, code) in [
            (FleetPosture::Nominal, 0),
            (FleetPosture::Degraded, 1),
            (FleetPosture::LockedOut, 2),
        ] {
            let text = m.format_prometheus("n", &snap(posture, false));
            assert!(
                text.contains(&format!("kirra_fleet_posture{{node_id=\"n\"}} {code}\n")),
                "posture {posture:?} must encode as {code}:\n{text}"
            );
        }
        let stale = m.format_prometheus("n", &snap(FleetPosture::LockedOut, true));
        assert!(stale.contains("kirra_posture_cache_stale{node_id=\"n\"} 1\n"));
    }

    /// Recorded events land on exactly the right labeled sample.
    #[test]
    fn recorded_events_render_on_the_right_labels() {
        let m = FleetSafetyMetrics::new();
        m.record_transition(&FleetPosture::Degraded);
        m.record_transition(&FleetPosture::Degraded);
        m.record_transition(&FleetPosture::LockedOut);
        m.record_gate_denial(GateDenialReason::PostureCacheStale);
        m.record_gate_denial(GateDenialReason::LockedOut);
        m.record_gate_denial(GateDenialReason::LockedOut);
        m.record_gate_denial(GateDenialReason::LockedOut);
        m.record_ha_promotion();

        let text = m.format_prometheus("n", &snap(FleetPosture::Nominal, false));
        for expected in [
            "kirra_posture_transitions_total{node_id=\"n\",posture=\"nominal\"} 0\n",
            "kirra_posture_transitions_total{node_id=\"n\",posture=\"degraded\"} 2\n",
            "kirra_posture_transitions_total{node_id=\"n\",posture=\"locked_out\"} 1\n",
            "kirra_gate_denials_total{node_id=\"n\",reason=\"posture_cache_stale\"} 1\n",
            "kirra_gate_denials_total{node_id=\"n\",reason=\"locked_out\"} 3\n",
            "kirra_gate_denials_total{node_id=\"n\",reason=\"unknown_command\"} 0\n",
            "kirra_ha_promotions_total{node_id=\"n\"} 1\n",
            "kirra_audit_write_drops_total{node_id=\"n\"} 11\n",
            "kirra_capture_drops_total{node_id=\"n\"} 12\n",
            "kirra_post_incident_write_failures_total{node_id=\"n\"} 13\n",
            "kirra_command_source_write_failures_total{node_id=\"n\"} 14\n",
        ] {
            assert!(text.contains(expected), "missing exact sample {expected:?} in:\n{text}");
        }
    }

    /// A node_id carrying label-breaking characters (`"`, `\`, newline) is
    /// escaped per the text-format rules, so an odd/hostile instance id can
    /// neither corrupt the exposition nor inject extra samples.
    #[test]
    fn node_id_is_escaped_in_label_values() {
        let m = FleetSafetyMetrics::new();
        let text = m.format_prometheus(
            "bad\"id\\with\nnewline",
            &snap(FleetPosture::Nominal, false),
        );
        assert!(
            text.contains("kirra_fleet_posture{node_id=\"bad\\\"id\\\\with\\nnewline\"} 0\n"),
            "node_id must be escaped for the quoted label value; got:\n{text}"
        );
        // No raw newline may survive inside a sample line (it would split
        // the sample and inject a bogus line).
        assert!(
            !text.lines().any(|l| l.starts_with("newline")),
            "a raw newline in node_id must not split a sample line:\n{text}"
        );
    }

    /// Label values are the stable snake_case codes (renaming one breaks
    /// every dashboard/alert built on it — pin the vocabulary).
    #[test]
    fn gate_denial_labels_are_pinned() {
        assert_eq!(GateDenialReason::UnknownCommand.as_label(), "unknown_command");
        assert_eq!(GateDenialReason::PostureCacheEmpty.as_label(), "posture_cache_empty");
        assert_eq!(GateDenialReason::PostureCacheStale.as_label(), "posture_cache_stale");
        assert_eq!(GateDenialReason::LockedOut.as_label(), "locked_out");
        assert_eq!(GateDenialReason::DegradedWriteDenied.as_label(), "degraded_write_denied");
        assert_eq!(GateDenialReason::HaFenced.as_label(), "ha_fenced");
    }
}
