//! The Mick **typed-destination endpoint core** — typed destination in, a
//! TRUSTED grounded target out, never a command and never a coordinate the
//! caller supplied.
//!
//! This is the live half of the #1293 foundation: the voice router classifies
//! a transcript deterministically, sends the typed [`DestinationRef`] here
//! (kind + the operator's words, NEVER coordinates), and this service asks the
//! trusted [`DestinationResolver`] — registries and live tracks — for the
//! actual pose.
//!
//! ```text
//!   voice → POST /destination {"kind":"named_place","query":"kitchen"}
//!         → DestinationRef::parse_json      (the ONE fail-closed parse)
//!         → DestinationResolver::resolve    (the ONLY source of coordinates)
//!         → Resolved  → latched on GET /destination/last  (seq advances)
//!         → anything else → 422 + a stable DEST_* code, latch UNCHANGED
//! ```
//!
//! 🔴 **Refusals never move anything.** Ambiguous / NotFound / Stale /
//! Unsupported all leave the latch and `seq` untouched, exactly like
//! [`crate::mick::IntentService`]'s fail-closed arm: with no new grounded
//! destination there is no goal, no plan, no proposal. There is no default
//! destination and no "head roughly that way" arm anywhere on this path.
//!
//! 🔴 **The latch is FRAME-EXPLICIT, and deliberately its own channel.** A
//! registry pose is in the registry's map frame; the existing
//! `GET /intent/last` channel is consumed (by `occy_doer`) as EGO-frame at
//! receipt. Publishing a map-frame pose there would be silently misread as
//! ego-relative and aim the robot at the wrong place — so grounded
//! destinations ride their own `GET /destination/last` channel carrying an
//! explicit [`GroundedFrame`], and a consumer must understand the frame
//! before it can use the pose. Wiring that consumption into the doer bridge
//! is the remaining deferred step (see `docs/hardware/R2_DESTINATIONS.md`).
//!
//! 🔴 **No perception is invented here.** Tracked-object destinations resolve
//! against the targets the CALLER supplies (a perception-aware bridge). The
//! voice router has no perception and supplies none, so an object request
//! from voice resolves [`ResolveOutcome::Stale`] — "the detector did not
//! look" is never "it isn't there", the same rule the object-goal channel
//! already holds.

use crate::destination::{
    DestinationRef, DestinationResolver, DestinationSource, EgoView, GroundedDestination,
    ResolveOutcome, TargetView,
};
use crate::net::RateLimiter;
use kirra_planner::{MickIntent, Pose};
use kirra_taj::object_goal::LabeledTarget;
use serde::Deserialize;

/// Destination rate bound (burst / steady per second). A plumbing bound: a
/// destination is one spoken request, so a runaway caller is shed cheaply.
pub const DESTINATION_RATE_BURST: f64 = 3.0;
pub const DESTINATION_RATE_PER_S: f64 = 1.0;

/// One typed-destination request. `destination` is the typed
/// [`DestinationRef`] wire object; everything else is OPTIONAL perception
/// context that only a perception-aware caller supplies.
#[derive(Deserialize, Default)]
pub struct DestinationRequest {
    /// The typed destination object (or that object as a JSON string).
    pub destination: serde_json::Value,
    /// Camera-detected labelled targets, ego frame — the tracked-object
    /// resolver's evidence. Absent → no evidence (→ `Stale`).
    #[serde(default)]
    pub targets: Vec<TargetReq>,
    /// Producer stamp of the frame `targets` came from. Absent while a
    /// tracked object is requested → STALE, never "not seen".
    #[serde(default)]
    pub targets_stamp_ms: Option<u64>,
    /// Consumer clock for the freshness check. Absent → the service clock.
    #[serde(default)]
    pub now_ms: Option<u64>,
    /// Ego pose the object sighting is lifted through. Absent → the identity
    /// pose, which makes an object result EGO-FRAME.
    #[serde(default)]
    pub ego: Option<EgoReq>,
}

/// One labelled camera target on the wire.
#[derive(Deserialize)]
pub struct TargetReq {
    pub label: String,
    pub x: f64,
    pub y: f64,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}
fn default_confidence() -> f32 {
    1.0
}

/// The caller's ego pose (world frame).
#[derive(Deserialize, Clone, Copy)]
pub struct EgoReq {
    pub x: f64,
    pub y: f64,
    pub heading: f64,
}

/// Which frame a grounded pose is expressed in. Carried explicitly on the
/// wire so a consumer can never mistake a map pose for an ego-relative one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroundedFrame {
    /// The registry's map frame — usable only by a consumer localized
    /// against that same `map_id`.
    Map(String),
    /// The frame the request's `ego` was expressed in (the identity ego pose
    /// → ego-relative: +X ahead, +Y left).
    EgoRelative,
}

impl GroundedFrame {
    /// The wire token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            GroundedFrame::Map(_) => "map",
            GroundedFrame::EgoRelative => "ego",
        }
    }
}

/// How completely a saved route was resolved. Phase 1 grounds ONLY the first
/// waypoint; the marker rides the wire and the logs so nothing downstream (or
/// spoken) can imply a whole route is under way.
pub const ROUTE_RESOLUTION_FIRST_WAYPOINT_ONLY: &str = "first_waypoint_only";
/// Not a route — no route-resolution claim applies.
pub const ROUTE_RESOLUTION_NONE: &str = "none";

/// An accepted, grounded destination, as latched for `GET /destination/last`.
#[derive(Clone, Debug, PartialEq)]
pub struct AcceptedDestination {
    /// Monotonic per-process sequence — a consumer applies a destination at
    /// most once by tracking the last seq it consumed (the same apply-once
    /// discipline as the intent latch).
    pub seq: u64,
    pub at_ms: u64,
    /// The registry id or matched detector label.
    pub destination_id: String,
    pub kind: &'static str,
    pub source: DestinationSource,
    pub frame: GroundedFrame,
    pub goal_pose: Pose,
    /// Full ordered waypoint list for a saved route (Phase 1: informational —
    /// `goal_pose` is the first waypoint only).
    pub route_waypoints: Vec<Pose>,
    pub route_resolution: &'static str,
}

impl AcceptedDestination {
    /// The `GET /destination/last` wire form.
    #[must_use]
    pub fn to_wire(&self) -> String {
        let map_id = match &self.frame {
            GroundedFrame::Map(id) => serde_json::Value::String(id.clone()),
            GroundedFrame::EgoRelative => serde_json::Value::Null,
        };
        serde_json::json!({
            "destination": {
                "destination_id": self.destination_id,
                "kind": self.kind,
                "source": self.source.as_str(),
                "frame": self.frame.as_str(),
                "map_id": map_id,
                "goal_pose": {
                    "x_m": self.goal_pose.x_m,
                    "y_m": self.goal_pose.y_m,
                    "heading_rad": self.goal_pose.heading_rad,
                },
                "route_waypoints": self.route_waypoints.iter().map(|w| serde_json::json!({
                    "x_m": w.x_m, "y_m": w.y_m, "heading_rad": w.heading_rad,
                })).collect::<Vec<_>>(),
                "route_resolution": self.route_resolution,
            },
            "seq": self.seq,
            "at_ms": self.at_ms,
        })
        .to_string()
    }

    /// The `POST /destination` success wire form — **admission only**. It
    /// deliberately carries NO pose: the caller (a voice frontend) has no use
    /// for coordinates and must not be able to read them back out, speak
    /// them, or log them. The grounded pose lives on the doer-facing latch.
    #[must_use]
    pub fn to_post_wire(&self) -> String {
        serde_json::json!({
            "ok": true,
            "seq": self.seq,
            "at_ms": self.at_ms,
            "outcome": "resolved",
            "kind": self.kind,
            "source": self.source.as_str(),
            "route_resolution": self.route_resolution,
        })
        .to_string()
    }

    /// The grounded destination as an ordinary governed [`MickIntent::GoTo`]
    /// — the #1293 conversion, so a consumer grounds it exactly like any
    /// other goal and nothing downstream can tell where it came from.
    ///
    /// The caller MUST have honoured [`Self::frame`] first: a `Map` pose is
    /// only a valid `GoTo` for a consumer localized in that map.
    #[must_use]
    pub fn to_goto(&self) -> MickIntent {
        MickIntent::GoTo {
            x_m: self.goal_pose.x_m,
            y_m: self.goal_pose.y_m,
        }
    }
}

/// A refusal: the stable machine code plus the operator sentence. Every
/// refusal means NO grounded destination and NO planner request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DestinationRefusal {
    pub code: String,
    pub sentence: String,
}

impl DestinationRefusal {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "ok": false,
            "error": self.code,
            "detail": self.sentence,
        })
        .to_string()
    }
}

/// The service core: trusted resolver + rate limit + frame-explicit latch.
/// Single-threaded, like the serve loop that drives it.
pub struct DestinationService {
    resolver: DestinationResolver,
    limiter: RateLimiter,
    last: Option<AcceptedDestination>,
    seq: u64,
}

impl DestinationService {
    #[must_use]
    pub fn new(resolver: DestinationResolver) -> Self {
        Self {
            resolver,
            limiter: RateLimiter::new(DESTINATION_RATE_BURST, DESTINATION_RATE_PER_S),
            last: None,
            seq: 0,
        }
    }

    /// The last accepted destination, if any (the `GET /destination/last`
    /// source).
    #[must_use]
    pub fn last(&self) -> Option<&AcceptedDestination> {
        self.last.as_ref()
    }

    /// Handle one typed-destination request at `now_ms`.
    ///
    /// On a resolved destination the grounded target is latched and `seq`
    /// advances. On ANY refusal the latch is untouched — fail-closed, so a
    /// consumer polling by seq sees nothing new and grounds nothing.
    pub fn handle(
        &mut self,
        req: &DestinationRequest,
        now_ms: u64,
    ) -> Result<AcceptedDestination, DestinationRefusal> {
        if !self.limiter.admit(now_ms) {
            return Err(DestinationRefusal {
                code: "DEST_RATE_LIMITED".to_string(),
                sentence: "Please wait a moment before asking for another destination.".to_string(),
            });
        }
        // Accept the object form or that object embedded as a JSON string —
        // both land in the ONE fail-closed destination parse.
        let raw = match req.destination.as_str() {
            Some(s) => s.to_string(),
            None => req.destination.to_string(),
        };
        let dref = DestinationRef::from_json(&raw).map_err(|code| DestinationRefusal {
            code: code.to_string(),
            sentence: "I couldn't validate that destination.".to_string(),
        })?;

        let targets: Vec<LabeledTarget> = req
            .targets
            .iter()
            .map(|t| LabeledTarget {
                label: t.label.clone(),
                x_m: t.x,
                y_m: t.y,
                confidence: t.confidence,
            })
            .collect();
        // An absent ego pose is the IDENTITY pose, which makes an object
        // result ego-relative — the honest frame for a caller that supplied
        // no localization, and the one the frame tag then reports.
        let ego = req.ego.unwrap_or(EgoReq {
            x: 0.0,
            y: 0.0,
            heading: 0.0,
        });
        let now = req.now_ms.unwrap_or(now_ms);

        let outcome = self.resolver.resolve(
            &dref,
            &TargetView {
                targets: &targets,
                stamp_ms: req.targets_stamp_ms,
            },
            EgoView {
                x_m: ego.x,
                y_m: ego.y,
                heading_rad: ego.heading,
            },
            now,
        );

        let grounded = match outcome {
            ResolveOutcome::Resolved(g) => g,
            refused => {
                // FAIL-CLOSED: nothing mutates. Not `last`, not `seq`. An
                // unchanged seq is what stops a consumer re-reading the
                // previous destination as newly requested.
                return Err(DestinationRefusal {
                    code: refused.code().to_string(),
                    sentence: refused.sentence(dref.query()),
                });
            }
        };

        let accepted = self.latch(&dref, grounded, now_ms);
        Ok(accepted)
    }

    fn latch(
        &mut self,
        dref: &DestinationRef,
        grounded: GroundedDestination,
        at_ms: u64,
    ) -> AcceptedDestination {
        // The frame comes from the RESOLUTION, not the request: a registry
        // hit is in that registry's map frame; a live sighting was lifted
        // through the supplied ego pose.
        let frame = match &grounded.map_id {
            Some(id) => GroundedFrame::Map(id.clone()),
            None => GroundedFrame::EgoRelative,
        };
        let route_resolution = if grounded.route_waypoints.is_empty() {
            ROUTE_RESOLUTION_NONE
        } else {
            // Phase 1: `goal_pose` is the FIRST waypoint. Nothing sequences
            // the remainder yet, and the marker says so on every wire read.
            ROUTE_RESOLUTION_FIRST_WAYPOINT_ONLY
        };
        self.seq += 1;
        let accepted = AcceptedDestination {
            seq: self.seq,
            at_ms,
            destination_id: grounded.destination_id,
            kind: kind_token(dref),
            source: grounded.source,
            frame,
            goal_pose: grounded.goal_pose,
            route_waypoints: grounded.route_waypoints,
            route_resolution,
        };
        self.last = Some(accepted.clone());
        accepted
    }
}

fn kind_token(dref: &DestinationRef) -> &'static str {
    match dref {
        DestinationRef::NamedPlace { .. } => "named_place",
        DestinationRef::SavedRoute { .. } => "saved_route",
        DestinationRef::TrackedObject { .. } => "tracked_object",
        DestinationRef::Address { .. } => "address",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::{ObjectPolicy, PlaceRegistry, RouteRegistry};

    const PLACES: &str = r#"{
        "version": 1, "map_id": "r2-lab-01",
        "places": [
            {"id":"kitchen","name":"Kitchen","aliases":["the kitchen"],
             "x_m":3.2,"y_m":-1.4,"heading_rad":0.0},
            {"id":"charging-dock","name":"Charging Dock","aliases":["the dock"],
             "x_m":0.5,"y_m":2.0,"heading_rad":0.0}
        ]}"#;
    const ROUTES: &str = r#"{
        "version": 1, "map_id": "r2-lab-01",
        "routes": [
            {"id":"route-a","name":"Route A","aliases":["route alpha"],
             "waypoints":[{"x_m":1.0,"y_m":0.0},{"x_m":2.0,"y_m":1.0}]}
        ]}"#;

    fn service() -> DestinationService {
        DestinationService::new(DestinationResolver {
            places: PlaceRegistry::from_json_str(PLACES).unwrap(),
            routes: RouteRegistry::from_json_str(ROUTES).unwrap(),
            object_policy: ObjectPolicy::default(),
        })
    }

    fn req(kind: &str, query: &str) -> DestinationRequest {
        DestinationRequest {
            destination: serde_json::json!({"kind": kind, "query": query}),
            ..Default::default()
        }
    }

    // ---- resolved destinations latch, frame-explicit ------------------------

    #[test]
    fn a_named_place_resolves_latches_and_advances_seq() {
        let mut svc = service();
        let a = svc
            .handle(&req("named_place", "the kitchen"), 5_000)
            .unwrap();
        assert_eq!(a.destination_id, "kitchen");
        assert_eq!(a.kind, "named_place");
        assert_eq!(a.source, DestinationSource::PlaceRegistry);
        assert_eq!(a.frame, GroundedFrame::Map("r2-lab-01".into()));
        assert_eq!(a.route_resolution, ROUTE_RESOLUTION_NONE);
        assert_eq!(a.seq, 1);
        assert_eq!(svc.last(), Some(&a));
        // The trusted registry pose — never anything the caller supplied.
        assert!((a.goal_pose.x_m - 3.2).abs() < 1e-12);
        // The #1293 conversion: an ordinary governed GoTo.
        assert_eq!(
            a.to_goto(),
            MickIntent::GoTo {
                x_m: 3.2,
                y_m: -1.4
            }
        );
    }

    /// The POST body a voice frontend sees carries ADMISSION only — no pose,
    /// no map id, nothing speakable about where the robot is going.
    #[test]
    fn the_post_wire_carries_no_coordinates_or_map_details() {
        let mut svc = service();
        let a = svc.handle(&req("named_place", "kitchen"), 1_000).unwrap();
        let body = a.to_post_wire();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["outcome"], "resolved");
        for leak in [
            "3.2",
            "-1.4",
            "x_m",
            "y_m",
            "goal_pose",
            "r2-lab-01",
            "map_id",
        ] {
            assert!(!body.contains(leak), "POST wire leaked {leak:?}: {body}");
        }
    }

    /// The doer-facing latch DOES carry the pose — with the frame named, so a
    /// map pose can never be silently read as ego-relative.
    #[test]
    fn the_latch_wire_names_its_frame() {
        let mut svc = service();
        let a = svc.handle(&req("named_place", "kitchen"), 1_000).unwrap();
        let v: serde_json::Value = serde_json::from_str(&a.to_wire()).unwrap();
        assert_eq!(v["destination"]["frame"], "map");
        assert_eq!(v["destination"]["map_id"], "r2-lab-01");
        assert_eq!(v["seq"], 1);
        assert!((v["destination"]["goal_pose"]["x_m"].as_f64().unwrap() - 3.2).abs() < 1e-12);
    }

    #[test]
    fn a_saved_route_is_marked_first_waypoint_only() {
        let mut svc = service();
        let a = svc
            .handle(&req("saved_route", "route alpha"), 1_000)
            .unwrap();
        assert_eq!(a.destination_id, "route-a");
        assert_eq!(a.route_resolution, ROUTE_RESOLUTION_FIRST_WAYPOINT_ONLY);
        assert_eq!(a.route_waypoints.len(), 2);
        assert!(
            (a.goal_pose.x_m - 1.0).abs() < 1e-12,
            "the goal is the FIRST waypoint"
        );
        // The marker rides the wire both ways, so nothing can imply the whole
        // route is under way.
        assert!(a
            .to_post_wire()
            .contains(ROUTE_RESOLUTION_FIRST_WAYPOINT_ONLY));
        assert!(a.to_wire().contains(ROUTE_RESOLUTION_FIRST_WAYPOINT_ONLY));
    }

    /// A tracked object supplied by a perception-aware caller resolves, and
    /// with no ego pose the result is honestly tagged EGO-relative.
    #[test]
    fn a_fresh_tracked_object_resolves_ego_relative_with_a_standoff() {
        let mut svc = service();
        let r = DestinationRequest {
            destination: serde_json::json!({"kind": "tracked_object", "class": "cup", "color": "red"}),
            targets: vec![TargetReq {
                label: "red cup".into(),
                x: 4.0,
                y: 0.0,
                confidence: 0.9,
            }],
            targets_stamp_ms: Some(1_000),
            now_ms: Some(1_000),
            ego: None,
        };
        let a = svc.handle(&r, 1_000).unwrap();
        assert_eq!(a.kind, "tracked_object");
        assert_eq!(a.frame, GroundedFrame::EgoRelative);
        assert_eq!(a.source, DestinationSource::ObjectTrack);
        // 4 m ahead, 0.75 m standoff → 3.25 m ahead: it approaches, never parks on it.
        assert!((a.goal_pose.x_m - 3.25).abs() < 1e-9, "{}", a.goal_pose.x_m);
    }

    // ---- every refusal is fail-closed --------------------------------------

    #[test]
    fn every_refusal_leaves_the_latch_and_seq_untouched() {
        let mut svc = service();
        // Establish a good latch first, so "untouched" is a real assertion.
        let good = svc.handle(&req("named_place", "kitchen"), 1_000).unwrap();
        assert_eq!(good.seq, 1);

        let cases: Vec<(DestinationRequest, &str)> = vec![
            (req("named_place", "garage"), "DEST_NOT_FOUND"),
            (req("saved_route", "route zulu"), "DEST_NOT_FOUND"),
            (
                req("address", "125 Main Street"),
                "DEST_UNSUPPORTED_ADDRESS",
            ),
            // No camera frame supplied → STALE ("the detector did not look"),
            // never "it isn't there".
            (req("tracked_object", "red cup"), "DEST_STALE"),
        ];
        // Space the requests past the token-bucket burst so a refusal is
        // charged to the resolver, not the rate limiter.
        for (i, (r, code)) in cases.into_iter().enumerate() {
            let t = 10_000 + i as u64 * 5_000;
            let e = svc.handle(&r, t).expect_err("must refuse");
            assert_eq!(e.code, code, "wrong code for case {i}");
            assert!(!e.sentence.is_empty(), "a refusal must be speakable");
            assert_eq!(svc.last(), Some(&good), "refusal {i} changed the latch");
            assert_eq!(svc.last().unwrap().seq, 1, "refusal {i} advanced seq");
        }
    }

    /// The core rule at the live seam: a caller cannot smuggle geometry
    /// through the language channel, and the refusal is LOUD.
    #[test]
    fn smuggled_coordinates_are_refused_with_the_distinct_code() {
        let mut svc = service();
        let r = DestinationRequest {
            destination: serde_json::json!({"kind":"named_place","query":"kitchen","x_m":9.0}),
            ..Default::default()
        };
        let e = svc.handle(&r, 1_000).expect_err("must refuse");
        assert_eq!(e.code, "DEST_COORDINATES_FORBIDDEN");
        assert!(svc.last().is_none(), "a smuggled request must not latch");
    }

    #[test]
    fn a_malformed_destination_object_fails_closed() {
        let mut svc = service();
        for bad in [
            serde_json::json!("drive somewhere nice"),
            serde_json::json!({"kind": "teleport", "query": "moon"}),
            serde_json::json!({"kind": "named_place", "query": "  "}),
            serde_json::json!({"kind": "tracked_object", "color": "red"}),
        ] {
            let r = DestinationRequest {
                destination: bad.clone(),
                ..Default::default()
            };
            let e = svc.handle(&r, 1_000).expect_err("must refuse");
            assert!(e.code.starts_with("DEST_"), "{bad} → {}", e.code);
            assert!(svc.last().is_none(), "{bad} latched");
        }
    }

    #[test]
    fn the_string_embedded_form_parses_identically() {
        let mut svc = service();
        let r = DestinationRequest {
            destination: serde_json::json!(r#"{"kind":"named_place","query":"kitchen"}"#),
            ..Default::default()
        };
        assert_eq!(svc.handle(&r, 1_000).unwrap().destination_id, "kitchen");
    }

    /// Two identical spoken requests produce two DISTINCT sequence numbers —
    /// a consumer applies each at most once and never re-applies an old one.
    #[test]
    fn each_accepted_request_advances_the_sequence_exactly_once() {
        let mut svc = service();
        let a = svc.handle(&req("named_place", "kitchen"), 1_000).unwrap();
        let b = svc.handle(&req("named_place", "the dock"), 10_000).unwrap();
        assert_eq!((a.seq, b.seq), (1, 2));
        assert_eq!(svc.last().unwrap().destination_id, "charging-dock");
    }

    #[test]
    fn a_flood_is_shed_before_the_resolver() {
        let mut svc = service();
        let mut shed = 0;
        for _ in 0..10 {
            if svc
                .handle(&req("named_place", "kitchen"), 10_000)
                .err()
                .is_some_and(|e| e.code == "DEST_RATE_LIMITED")
            {
                shed += 1;
            }
        }
        assert!(shed >= 6, "the burst bound must shed a flood: {shed}");
    }

    #[test]
    fn an_empty_resolver_refuses_registry_kinds_without_a_latch() {
        let mut svc = DestinationService::new(DestinationResolver::default());
        let e = svc
            .handle(&req("named_place", "kitchen"), 1_000)
            .expect_err("no registry must refuse");
        assert_eq!(e.code, "DEST_NO_REGISTRY");
        assert!(svc.last().is_none());
    }
}
