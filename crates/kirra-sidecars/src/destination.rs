//! Typed destination resolution — **language chooses the destination TYPE; a
//! trusted resolver chooses the actual coordinates.**
//!
//! The problem this module exists to close: "drive to the kitchen" must never
//! become an LLM-invented coordinate pair. An LLM (or the deterministic voice
//! router) may only pick a typed [`DestinationRef`] — a KIND plus the
//! operator's words — and the [`DestinationResolver`] grounds it against
//! trusted sources:
//!
//! | kind             | trusted source                              | outcome        |
//! |------------------|---------------------------------------------|----------------|
//! | `named_place`    | operator-calibrated [`PlaceRegistry`] JSON  | registry pose  |
//! | `saved_route`    | operator-calibrated [`RouteRegistry`] JSON  | waypoint list  |
//! | `tracked_object` | live camera targets (`kirra_taj::object_goal`) | standoff pose |
//! | `address`        | none (no geocoder exists)                   | **Unsupported**|
//!
//! Every resolution returns an explicit [`ResolveOutcome`] — Resolved /
//! Ambiguous / NotFound / Stale / Unsupported — never an `Option`, so a caller
//! cannot conflate "no such place" with "the camera view is stale" with "we
//! don't do street addresses". Every non-Resolved arm is a REFUSAL to drive:
//! the caller speaks the reason ([`ResolveOutcome::sentence`]) instead of
//! moving.
//!
//! 🔴 **Coordinates never enter through language.** [`DestinationRef::parse_json`]
//! is the ONE fail-closed parser for destination JSON: it rejects any object
//! carrying a coordinate-shaped key outright (`DEST_COORDINATES_FORBIDDEN`),
//! and [`destination_schema`] (the constrained-decode form handed to an LLM
//! backend) has NO numeric property at all — the model structurally cannot
//! emit a coordinate even before the parse refuses one.
//!
//! 🔴 **A grounded destination asserts nothing about drivability.** Like the
//! `object_goal` channel this generalizes, a [`GroundedDestination`] is a
//! DESTINATION ONLY: the caller grounds it as an ordinary `MickIntent::GoTo`,
//! Occy plans inside the lidar corridor, and the KIRRA checker bounds the
//! trajectory — a registered place behind an obstacle produces a robot that
//! stops short, never one that drives at the registry entry.
//!
//! This module lives in `kirra-sidecars` deliberately: the crate is FENCED by
//! `ci/check_mick_actuation_fence.py`, so "the resolver has no dependency
//! route to the release-token mint, the serial seam, or any ROS/DDS
//! transport" is an existing CI invariant, not a promise.

use std::collections::HashMap;

use kirra_planner::{extract_first_json_object, Pose};
use kirra_taj::object_goal::{
    resolve_object_goal, GoalRefusal, LabeledTarget, ObjectGoal, DEFAULT_TIE_EPSILON_M,
};
use serde::Deserialize;

/// Upper bound on a destination query (characters) — a plumbing bound against
/// prompt/transcript pathologies, not a safety number.
pub const MAX_QUERY_CHARS: usize = 120;

/// The one registry file format version this build reads. A future version is
/// REFUSED at load (fail-closed), never best-effort parsed.
pub const DEST_REGISTRY_VERSION: u64 = 1;

/// Default freshness budget for a tracked-object destination (ms). Wider than
/// the `object_goal` channel's 750 ms because a spoken "drive to the red cup"
/// round-trips STT + routing before it reaches the resolver; still tight
/// enough that a moved object cannot be chased on an old sighting.
pub const DEFAULT_OBJECT_MAX_AGE_MS: u64 = 2_000;

/// Default minimum detector confidence for a tracked-object destination.
pub const DEFAULT_OBJECT_MIN_CONFIDENCE: f32 = 0.70;

/// Default standoff (m): the grounded goal stops this far SHORT of the object
/// along the ego→object ray — the robot approaches a thing, it does not park
/// on top of it.
pub const DEFAULT_OBJECT_STANDOFF_M: f64 = 0.75;

/// Registry parse bounds (fail-closed: an oversized file is refused, never
/// truncated). Plumbing bounds sized far above any real deployment.
const MAX_REGISTRY_ENTRIES: usize = 4_096;
const MAX_ALIASES_PER_ENTRY: usize = 32;
const MAX_ROUTE_WAYPOINTS: usize = 1_024;

// ---------------------------------------------------------------------------
// The typed destination reference — what LANGUAGE is allowed to choose.
// ---------------------------------------------------------------------------

/// A typed destination request: a KIND plus the operator's words. Carries no
/// coordinates by construction — the resolver supplies those.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationRef {
    /// A place from the operator-calibrated place registry ("the kitchen").
    NamedPlace { query: String },
    /// A saved multi-waypoint route from the route registry ("route A").
    SavedRoute { query: String },
    /// A live camera-tracked labelled object ("the red cup").
    TrackedObject { query: String },
    /// A street address ("125 Main Street") — a typed seam that resolves
    /// [`ResolveOutcome::Unsupported`] until a real, trusted geocoding source
    /// exists. Deliberately present so address requests fail CLOSED with an
    /// honest reason instead of being mis-bucketed into another kind.
    Address { query: String },
}

/// The wire form: `{"kind":"named_place","query":"kitchen"}`. Strict —
/// unknown fields are a parse error, so nothing can ride alongside the query.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DestinationJson {
    kind: String,
    query: String,
}

/// Keys whose presence in a destination object means someone (a model, a
/// caller) tried to smuggle geometry through the language channel. Refused
/// with a DISTINCT code so the violation is loud in telemetry, not folded
/// into generic parse noise.
const COORDINATE_KEYS: &[&str] = &[
    "x_m",
    "y_m",
    "x",
    "y",
    "lat",
    "lon",
    "latitude",
    "longitude",
    "heading_rad",
    "pose",
    "waypoints",
];

/// The tracked-object attribute keys. Admitted ONLY on `tracked_object`, and
/// only in place of `query` — never alongside it (two spellings of the same
/// field is an ambiguity, not a merge).
const OBJECT_ATTRIBUTE_KEYS: &[&str] = &["class", "color"];

/// Compose `{"class":"cup","color":"red"}` into the query `"red cup"`.
///
/// Returns `Ok(None)` when the object carries no attribute keys (the ordinary
/// `query` form). Fail-closed on every misuse: attributes on a non-object
/// kind, attributes alongside `query`, a missing/blank `class`, or a
/// non-string attribute.
fn compose_object_query(
    obj: &serde_json::Map<String, serde_json::Value>,
    kind: &str,
) -> Result<Option<String>, &'static str> {
    let has_attrs = OBJECT_ATTRIBUTE_KEYS.iter().any(|k| obj.contains_key(*k));
    if !has_attrs {
        return Ok(None);
    }
    if kind != "tracked_object" {
        // A place/route/address does not have a class or a colour. Refusing
        // keeps each kind's wire shape exactly one thing.
        return Err("DEST_JSON_PARSE_ERROR");
    }
    if obj.contains_key("query") {
        return Err("DEST_JSON_PARSE_ERROR");
    }
    // The attribute form bypasses serde's `deny_unknown_fields`, so it
    // enforces the same closed key set itself — nothing may ride alongside.
    if obj
        .keys()
        .any(|k| k != "kind" && !OBJECT_ATTRIBUTE_KEYS.contains(&k.as_str()))
    {
        return Err("DEST_JSON_PARSE_ERROR");
    }
    let attr = |key: &str| -> Result<Option<&str>, &'static str> {
        match obj.get(key) {
            None => Ok(None),
            Some(serde_json::Value::String(s)) => Ok(Some(s.trim())),
            Some(_) => Err("DEST_JSON_PARSE_ERROR"),
        }
    };
    let class = attr("class")?.unwrap_or("");
    if class.is_empty() {
        // A colour alone ("the red one") names no thing to resolve.
        return Err("DEST_EMPTY_QUERY");
    }
    Ok(Some(match attr("color")? {
        Some(color) if !color.is_empty() => format!("{color} {class}"),
        _ => class.to_string(),
    }))
}

impl DestinationRef {
    /// Parse destination JSON — THE one fail-closed parser for the typed
    /// destination wire form. Tolerant of LLM framing (fences/preamble) via
    /// the same extractor as the intent parse; strict on everything else:
    /// unknown kind, unknown fields, empty/oversized query, and — loudest —
    /// any coordinate-shaped key (`DEST_COORDINATES_FORBIDDEN`).
    ///
    /// Returns the typed ref AND the exact accepted `{…}` slice (the
    /// re-publishable wire artifact, mirroring `MickIntent::parse_llm_json`).
    pub fn parse_json(raw: &str) -> Result<(Self, &str), &'static str> {
        let json = extract_first_json_object(raw).ok_or("DEST_JSON_PARSE_ERROR")?;
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|_| "DEST_JSON_PARSE_ERROR")?;
        let obj = value.as_object().ok_or("DEST_JSON_PARSE_ERROR")?;
        if obj.keys().any(|k| COORDINATE_KEYS.contains(&k.as_str())) {
            return Err("DEST_COORDINATES_FORBIDDEN");
        }
        // The tracked-object ATTRIBUTE form (`class` + optional `color`) is a
        // second accepted spelling of the SAME typed request, for the voice
        // router's deliberately small object grammar. It composes into the
        // one `query` the resolver already matches — there is no second
        // resolution path, and no other kind may carry these keys.
        let kind = obj
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or("DEST_JSON_PARSE_ERROR")?;
        let composed = compose_object_query(obj, kind)?;
        let parsed: DestinationJson = match composed {
            Some(query) => DestinationJson {
                kind: kind.to_string(),
                query,
            },
            None => serde_json::from_str(json).map_err(|_| "DEST_JSON_PARSE_ERROR")?,
        };
        let query = parsed.query.trim();
        if query.is_empty() {
            return Err("DEST_EMPTY_QUERY");
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err("DEST_QUERY_TOO_LONG");
        }
        let query = query.to_string();
        let dref = match parsed.kind.as_str() {
            "named_place" => DestinationRef::NamedPlace { query },
            "saved_route" => DestinationRef::SavedRoute { query },
            "tracked_object" => DestinationRef::TrackedObject { query },
            "address" => DestinationRef::Address { query },
            _ => return Err("DEST_UNKNOWN_KIND"),
        };
        Ok((dref, json))
    }

    /// As [`parse_json`](Self::parse_json), discarding the slice.
    pub fn from_json(raw: &str) -> Result<Self, &'static str> {
        Self::parse_json(raw).map(|(dref, _)| dref)
    }

    /// The wire kind token.
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            DestinationRef::NamedPlace { .. } => "named_place",
            DestinationRef::SavedRoute { .. } => "saved_route",
            DestinationRef::TrackedObject { .. } => "tracked_object",
            DestinationRef::Address { .. } => "address",
        }
    }

    /// The operator's words.
    #[must_use]
    pub fn query(&self) -> &str {
        match self {
            DestinationRef::NamedPlace { query }
            | DestinationRef::SavedRoute { query }
            | DestinationRef::TrackedObject { query }
            | DestinationRef::Address { query } => query,
        }
    }
}

/// Normalize a query / registry key for matching: ASCII-lowercase,
/// non-alphanumerics collapse to single spaces, trimmed. Mirrors the voice
/// router's `normalize` so "The Kitchen!" and "the kitchen" are one key.
#[must_use]
pub fn normalize_query(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending_space = true;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Registries — the trusted, operator-calibrated coordinate sources.
// ---------------------------------------------------------------------------

/// One calibrated named place.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedPlace {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    /// Pose in the registry's map frame.
    pub pose: Pose,
}

/// One saved route: an ordered waypoint list in the registry's map frame.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedRoute {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub waypoints: Vec<Pose>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceFile {
    version: u64,
    map_id: String,
    places: Vec<PlaceEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceEntry {
    id: String,
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    x_m: f64,
    y_m: f64,
    #[serde(default)]
    heading_rad: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteFile {
    version: u64,
    map_id: String,
    routes: Vec<RouteEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteEntry {
    id: String,
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    waypoints: Vec<WaypointEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaypointEntry {
    x_m: f64,
    y_m: f64,
    #[serde(default)]
    heading_rad: f64,
}

/// Validate one entry's identity + collect its normalized lookup keys into
/// `keys`, refusing (fail-closed) on empties, duplicates across the WHOLE
/// registry (ids, names, and aliases share one key space, so a lookup can
/// never be ambiguous by construction), or too many aliases.
fn index_entry_keys(
    keys: &mut HashMap<String, usize>,
    idx: usize,
    id: &str,
    name: &str,
    aliases: &[String],
    what: &str,
) -> Result<(), String> {
    if aliases.len() > MAX_ALIASES_PER_ENTRY {
        return Err(format!(
            "{what} {id:?}: {} aliases exceeds the {MAX_ALIASES_PER_ENTRY} bound",
            aliases.len()
        ));
    }
    for raw in [id, name]
        .into_iter()
        .chain(aliases.iter().map(|a| a.as_str()))
    {
        let key = normalize_query(raw);
        if key.is_empty() {
            return Err(format!(
                "{what} {id:?}: key {raw:?} is empty after normalization"
            ));
        }
        if let Some(prev) = keys.insert(key.clone(), idx) {
            if prev != idx {
                return Err(format!(
                    "{what} {id:?}: key {raw:?} collides with another entry — \
                     ids, names and aliases must be unique across the registry"
                ));
            }
        }
    }
    Ok(())
}

fn check_finite_pose(x: f64, y: f64, heading: f64, what: &str, id: &str) -> Result<(), String> {
    if x.is_finite() && y.is_finite() && heading.is_finite() {
        Ok(())
    } else {
        Err(format!("{what} {id:?}: non-finite coordinate"))
    }
}

/// The named-place registry: a validated, immutable lookup table loaded once
/// at boot from operator-calibrated JSON. Load is FAIL-CLOSED — any defect
/// (wrong version, empty/duplicate keys, non-finite coordinates, unknown
/// fields, missing `map_id`) refuses the whole file with a description; a
/// partially-usable registry is never constructed.
#[derive(Debug, Clone, Default)]
pub struct PlaceRegistry {
    map_id: String,
    places: Vec<NamedPlace>,
    keys: HashMap<String, usize>,
}

impl PlaceRegistry {
    /// An empty registry (no file configured): every lookup refuses with
    /// `DEST_NO_REGISTRY`.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parse + validate a registry file. Fail-closed on any defect.
    pub fn from_json_str(raw: &str) -> Result<Self, String> {
        let file: PlaceFile =
            serde_json::from_str(raw).map_err(|e| format!("place registry: {e}"))?;
        if file.version != DEST_REGISTRY_VERSION {
            return Err(format!(
                "place registry: version {} is not the supported version {DEST_REGISTRY_VERSION}",
                file.version
            ));
        }
        if normalize_query(&file.map_id).is_empty() {
            return Err("place registry: map_id is required and must be non-empty".to_string());
        }
        if file.places.len() > MAX_REGISTRY_ENTRIES {
            return Err(format!(
                "place registry: {} entries exceeds the {MAX_REGISTRY_ENTRIES} bound",
                file.places.len()
            ));
        }
        let mut keys = HashMap::new();
        let mut places = Vec::with_capacity(file.places.len());
        for (idx, e) in file.places.iter().enumerate() {
            check_finite_pose(e.x_m, e.y_m, e.heading_rad, "place", &e.id)?;
            index_entry_keys(&mut keys, idx, &e.id, &e.name, &e.aliases, "place")?;
            places.push(NamedPlace {
                id: e.id.clone(),
                name: e.name.clone(),
                aliases: e.aliases.clone(),
                pose: Pose {
                    x_m: e.x_m,
                    y_m: e.y_m,
                    heading_rad: e.heading_rad,
                },
            });
        }
        Ok(Self {
            map_id: file.map_id,
            places,
            keys,
        })
    }

    /// Exact lookup by normalized id / name / alias. Registry-load uniqueness
    /// makes a hit structurally unambiguous.
    #[must_use]
    pub fn lookup(&self, query: &str) -> Option<&NamedPlace> {
        self.keys
            .get(&normalize_query(query))
            .map(|&i| &self.places[i])
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    #[must_use]
    pub fn map_id(&self) -> &str {
        &self.map_id
    }

    /// Display names (for prompt rendering / the CLI). NAMES ONLY — a
    /// coordinate never leaves the registry through this.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.places.iter().map(|p| p.name.clone()).collect()
    }
}

/// The saved-route registry — same load/validation contract as
/// [`PlaceRegistry`], plus: every route needs 1..=[`MAX_ROUTE_WAYPOINTS`]
/// finite waypoints.
#[derive(Debug, Clone, Default)]
pub struct RouteRegistry {
    map_id: String,
    routes: Vec<SavedRoute>,
    keys: HashMap<String, usize>,
}

impl RouteRegistry {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parse + validate a route registry file. Fail-closed on any defect.
    pub fn from_json_str(raw: &str) -> Result<Self, String> {
        let file: RouteFile =
            serde_json::from_str(raw).map_err(|e| format!("route registry: {e}"))?;
        if file.version != DEST_REGISTRY_VERSION {
            return Err(format!(
                "route registry: version {} is not the supported version {DEST_REGISTRY_VERSION}",
                file.version
            ));
        }
        if normalize_query(&file.map_id).is_empty() {
            return Err("route registry: map_id is required and must be non-empty".to_string());
        }
        if file.routes.len() > MAX_REGISTRY_ENTRIES {
            return Err(format!(
                "route registry: {} entries exceeds the {MAX_REGISTRY_ENTRIES} bound",
                file.routes.len()
            ));
        }
        let mut keys = HashMap::new();
        let mut routes = Vec::with_capacity(file.routes.len());
        for (idx, e) in file.routes.iter().enumerate() {
            if e.waypoints.is_empty() || e.waypoints.len() > MAX_ROUTE_WAYPOINTS {
                return Err(format!(
                    "route {:?}: {} waypoints outside 1..={MAX_ROUTE_WAYPOINTS}",
                    e.id,
                    e.waypoints.len()
                ));
            }
            for w in &e.waypoints {
                check_finite_pose(w.x_m, w.y_m, w.heading_rad, "route", &e.id)?;
            }
            index_entry_keys(&mut keys, idx, &e.id, &e.name, &e.aliases, "route")?;
            routes.push(SavedRoute {
                id: e.id.clone(),
                name: e.name.clone(),
                aliases: e.aliases.clone(),
                waypoints: e
                    .waypoints
                    .iter()
                    .map(|w| Pose {
                        x_m: w.x_m,
                        y_m: w.y_m,
                        heading_rad: w.heading_rad,
                    })
                    .collect(),
            });
        }
        Ok(Self {
            map_id: file.map_id,
            routes,
            keys,
        })
    }

    /// Exact lookup by normalized id / name / alias.
    #[must_use]
    pub fn lookup(&self, query: &str) -> Option<&SavedRoute> {
        self.keys
            .get(&normalize_query(query))
            .map(|&i| &self.routes[i])
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    #[must_use]
    pub fn map_id(&self) -> &str {
        &self.map_id
    }

    /// Display names only — never coordinates.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.routes.iter().map(|r| r.name.clone()).collect()
    }
}

// ---------------------------------------------------------------------------
// Object policy + the resolver.
// ---------------------------------------------------------------------------

/// Tracked-object resolution policy — validated at construction so a
/// malformed configuration aborts boot rather than silently defaulting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObjectPolicy {
    /// A sighting older than this (ms) is [`ResolveOutcome::Stale`].
    pub max_age_ms: u64,
    /// Matches below this confidence never become a destination.
    pub min_confidence: f32,
    /// The grounded goal stops this far short of the object (m).
    pub standoff_m: f64,
}

impl Default for ObjectPolicy {
    fn default() -> Self {
        Self {
            max_age_ms: DEFAULT_OBJECT_MAX_AGE_MS,
            min_confidence: DEFAULT_OBJECT_MIN_CONFIDENCE,
            standoff_m: DEFAULT_OBJECT_STANDOFF_M,
        }
    }
}

impl ObjectPolicy {
    /// Bounds-checked constructor. `max_age_ms >= 1`; confidence in `[0, 1]`;
    /// standoff finite in `[0, 5]` m (a standoff wider than that is a config
    /// typo, not a policy).
    pub fn validated(
        max_age_ms: u64,
        min_confidence: f32,
        standoff_m: f64,
    ) -> Result<Self, String> {
        if max_age_ms == 0 {
            return Err("object policy: max_age_ms must be >= 1".to_string());
        }
        if !min_confidence.is_finite() || !(0.0..=1.0).contains(&min_confidence) {
            return Err(format!(
                "object policy: min_confidence {min_confidence} outside [0, 1]"
            ));
        }
        if !standoff_m.is_finite() || !(0.0..=5.0).contains(&standoff_m) {
            return Err(format!(
                "object policy: standoff_m {standoff_m} outside [0, 5]"
            ));
        }
        Ok(Self {
            max_age_ms,
            min_confidence,
            standoff_m,
        })
    }
}

/// Which trusted source produced a grounded destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationSource {
    PlaceRegistry,
    RouteRegistry,
    ObjectTrack,
}

impl DestinationSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DestinationSource::PlaceRegistry => "place_registry",
            DestinationSource::RouteRegistry => "route_registry",
            DestinationSource::ObjectTrack => "object_track",
        }
    }
}

/// A resolver-grounded navigation target. A DESTINATION ONLY — it asserts
/// nothing about drivability; the governed planner path adjudicates the way
/// there exactly as for any other goal.
#[derive(Debug, Clone, PartialEq)]
pub struct GroundedDestination {
    /// The registry id (place/route) or the matched detector label (object).
    pub destination_id: String,
    /// The registry's map frame id. `None` for an object track — a live
    /// sighting is in the request's world frame and has no persistent map
    /// identity. A caller that carries map identity MUST check this against
    /// its active map before acting.
    pub map_id: Option<String>,
    /// The goal pose (for a route: its FIRST waypoint — sequencing the rest
    /// is the mission layer's job, see `route_waypoints`).
    pub goal_pose: Pose,
    /// The full ordered waypoint list (empty unless a saved route).
    pub route_waypoints: Vec<Pose>,
    pub source: DestinationSource,
    /// The resolver clock at resolution (the caller's `now_ms`).
    pub resolved_at_ms: u64,
}

/// The explicit resolution outcome. Deliberately an enum, not an `Option`:
/// each non-Resolved arm is a DIFFERENT refusal with its own stable code and
/// operator sentence, and collapsing them would let a caller mistake "stale
/// camera" for "no such place".
#[derive(Debug, Clone, PartialEq)]
pub enum ResolveOutcome {
    Resolved(GroundedDestination),
    /// More than one equally-good match — refuse rather than silently pick.
    /// `candidates` (possibly empty) names what tied, for narration.
    Ambiguous {
        candidates: Vec<String>,
    },
    /// Nothing matched; `code` carries the specific reason
    /// (`DEST_NOT_FOUND` / `DEST_NO_REGISTRY` / `DEST_NOT_SEEN` /
    /// `DEST_LOW_CONFIDENCE` / `DEST_BEHIND_EGO` / `DEST_NON_FINITE`).
    NotFound {
        code: &'static str,
    },
    /// The evidence is too old (or absent) to drive on.
    Stale,
    /// The destination kind has no trusted resolution source (`address`
    /// today); `code` carries the specific seam.
    Unsupported {
        code: &'static str,
    },
}

impl ResolveOutcome {
    /// Stable machine code — the telemetry/seam-rejection token.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            ResolveOutcome::Resolved(_) => "DEST_RESOLVED",
            ResolveOutcome::Ambiguous { .. } => "DEST_AMBIGUOUS",
            ResolveOutcome::NotFound { code } | ResolveOutcome::Unsupported { code } => code,
            ResolveOutcome::Stale => "DEST_STALE",
        }
    }

    /// One operator-facing sentence for a refusal — the robot SAYS why it is
    /// not driving instead of silently holding. (Resolved has no refusal
    /// sentence; it returns an empty string.)
    #[must_use]
    pub fn sentence(&self, query: &str) -> String {
        match self {
            ResolveOutcome::Resolved(_) => String::new(),
            ResolveOutcome::Ambiguous { candidates } => {
                if candidates.is_empty() {
                    format!("I can see more than one {query} — which one did you mean?")
                } else {
                    format!(
                        "More than one destination matches {query} ({}) — which one did you mean?",
                        candidates.join(", ")
                    )
                }
            }
            ResolveOutcome::NotFound { code } => match *code {
                "DEST_NO_REGISTRY" => {
                    format!("I don't have any saved places or routes yet, so I can't find {query}.")
                }
                "DEST_NOT_SEEN" => {
                    format!("I can't see {query} from here, so I'm staying put.")
                }
                "DEST_LOW_CONFIDENCE" => {
                    format!("I might be seeing {query}, but not clearly enough to drive to it.")
                }
                "DEST_BEHIND_EGO" => {
                    format!("{query} is behind me, and I only drive forward to a target.")
                }
                "DEST_NON_FINITE" => {
                    format!("My reading of {query} doesn't make sense, so I'm not moving.")
                }
                _ => format!("I don't know a destination called {query}."),
            },
            ResolveOutcome::Stale => {
                format!("My view of {query} is stale, so I won't drive on it.")
            }
            ResolveOutcome::Unsupported { .. } => {
                "I can't navigate to street addresses yet, so I'm staying put.".to_string()
            }
        }
    }
}

/// The live camera-target view a resolution runs against (the tracked-object
/// arm's evidence). `stamp_ms: None` means "the detector did not look", which
/// is Stale — never "it isn't there".
pub struct TargetView<'a> {
    pub targets: &'a [LabeledTarget],
    pub stamp_ms: Option<u64>,
}

/// The ego pose in the request's world frame (for grounding an ego-frame
/// object sighting and applying the standoff).
#[derive(Debug, Clone, Copy)]
pub struct EgoView {
    pub x_m: f64,
    pub y_m: f64,
    pub heading_rad: f64,
}

/// The trusted resolver: registries + object policy. Language hands it a
/// [`DestinationRef`]; it alone chooses coordinates.
#[derive(Debug, Clone, Default)]
pub struct DestinationResolver {
    pub places: PlaceRegistry,
    pub routes: RouteRegistry,
    pub object_policy: ObjectPolicy,
}

impl DestinationResolver {
    /// Resolve one typed destination against the trusted sources. Pure: all
    /// evidence and clocks are parameters.
    #[must_use]
    pub fn resolve(
        &self,
        dref: &DestinationRef,
        view: &TargetView<'_>,
        ego: EgoView,
        now_ms: u64,
    ) -> ResolveOutcome {
        match dref {
            DestinationRef::NamedPlace { query } => {
                if self.places.is_empty() {
                    return ResolveOutcome::NotFound {
                        code: "DEST_NO_REGISTRY",
                    };
                }
                match self.places.lookup(query) {
                    Some(place) => ResolveOutcome::Resolved(GroundedDestination {
                        destination_id: place.id.clone(),
                        map_id: Some(self.places.map_id().to_string()),
                        goal_pose: place.pose,
                        route_waypoints: Vec::new(),
                        source: DestinationSource::PlaceRegistry,
                        resolved_at_ms: now_ms,
                    }),
                    None => ResolveOutcome::NotFound {
                        code: "DEST_NOT_FOUND",
                    },
                }
            }
            DestinationRef::SavedRoute { query } => {
                if self.routes.is_empty() {
                    return ResolveOutcome::NotFound {
                        code: "DEST_NO_REGISTRY",
                    };
                }
                match self.routes.lookup(query) {
                    Some(route) => ResolveOutcome::Resolved(GroundedDestination {
                        destination_id: route.id.clone(),
                        map_id: Some(self.routes.map_id().to_string()),
                        goal_pose: route.waypoints[0],
                        route_waypoints: route.waypoints.clone(),
                        source: DestinationSource::RouteRegistry,
                        resolved_at_ms: now_ms,
                    }),
                    None => ResolveOutcome::NotFound {
                        code: "DEST_NOT_FOUND",
                    },
                }
            }
            DestinationRef::TrackedObject { query } => {
                self.resolve_object(query, view, ego, now_ms)
            }
            DestinationRef::Address { .. } => ResolveOutcome::Unsupported {
                code: "DEST_UNSUPPORTED_ADDRESS",
            },
        }
    }

    /// The tracked-object arm: delegate the match/freshness/confidence/tie
    /// policy to the proven `object_goal` resolver, then apply the standoff
    /// and lift into the world frame.
    fn resolve_object(
        &self,
        query: &str,
        view: &TargetView<'_>,
        ego: EgoView,
        now_ms: u64,
    ) -> ResolveOutcome {
        let goal = match resolve_object_goal(
            query,
            view.targets,
            view.stamp_ms,
            now_ms,
            self.object_policy.max_age_ms,
            self.object_policy.min_confidence,
            DEFAULT_TIE_EPSILON_M,
        ) {
            Ok(goal) => goal,
            Err(GoalRefusal::Stale) => return ResolveOutcome::Stale,
            Err(GoalRefusal::Ambiguous) => {
                return ResolveOutcome::Ambiguous {
                    candidates: Vec::new(),
                }
            }
            Err(GoalRefusal::NoRequestedLabel) | Err(GoalRefusal::NotSeen) => {
                return ResolveOutcome::NotFound {
                    code: "DEST_NOT_SEEN",
                }
            }
            Err(GoalRefusal::LowConfidence) => {
                return ResolveOutcome::NotFound {
                    code: "DEST_LOW_CONFIDENCE",
                }
            }
            Err(GoalRefusal::BehindEgo) => {
                return ResolveOutcome::NotFound {
                    code: "DEST_BEHIND_EGO",
                }
            }
            Err(GoalRefusal::NonFinite) => {
                return ResolveOutcome::NotFound {
                    code: "DEST_NON_FINITE",
                }
            }
        };

        // Standoff in the EGO frame: pull the goal back along the ego→object
        // ray so the robot approaches the thing, never parks on it. Within
        // the standoff already → the goal is the ego's own position (a no-op
        // approach), still Resolved so the caller can narrate "I'm already
        // there".
        let range = goal.x_m.hypot(goal.y_m);
        let scale = if range <= self.object_policy.standoff_m {
            0.0
        } else {
            (range - self.object_policy.standoff_m) / range
        };
        let standoff_goal = ObjectGoal {
            label: goal.label.clone(),
            x_m: goal.x_m * scale,
            y_m: goal.y_m * scale,
            confidence: goal.confidence,
        };
        let (x_w, y_w) = standoff_goal.to_world(ego.x_m, ego.y_m, ego.heading_rad);
        // Face the object from the standoff point.
        let heading_w = ego.heading_rad + goal.y_m.atan2(goal.x_m);

        ResolveOutcome::Resolved(GroundedDestination {
            destination_id: goal.label,
            map_id: None,
            goal_pose: Pose {
                x_m: x_w,
                y_m: y_w,
                heading_rad: heading_w,
            },
            route_waypoints: Vec::new(),
            source: DestinationSource::ObjectTrack,
            resolved_at_ms: now_ms,
        })
    }
}

// ---------------------------------------------------------------------------
// Boot configuration — ONE config path, shared by every binary that resolves.
// ---------------------------------------------------------------------------

/// Read one env var, parsed by `parse`, defaulting when absent/empty.
/// Malformed → `Err` (the caller aborts startup) — never a silent default.
fn env_parsed<T>(
    key: &str,
    default: T,
    parse: impl FnOnce(&str) -> Option<T>,
) -> Result<T, String> {
    match std::env::var(key) {
        Ok(raw) if !raw.trim().is_empty() => {
            parse(raw.trim()).ok_or_else(|| format!("{key}: malformed value {raw:?}"))
        }
        _ => Ok(default),
    }
}

fn registry_from_env<T>(
    key: &str,
    empty: T,
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> Result<T, String> {
    match std::env::var(key) {
        Ok(path) if !path.trim().is_empty() => {
            let path = path.trim();
            let raw = std::fs::read_to_string(path).map_err(|e| format!("{key} {path:?}: {e}"))?;
            parse(&raw).map_err(|e| format!("{key} {path:?}: {e}"))
        }
        _ => Ok(empty),
    }
}

/// Build the trusted destination resolver from the environment at boot.
///
/// Shared by EVERY binary that resolves destinations (`planner_service`,
/// `mick_service`) so a fleet cannot end up with two processes disagreeing
/// about what "the kitchen" means. Fail-closed throughout: a configured
/// registry path that is unreadable or fails validation, or a malformed
/// policy knob, is an `Err` the caller turns into a startup abort — a service
/// with a half-loaded registry must not come up looking healthy.
pub fn resolver_from_env() -> Result<DestinationResolver, String> {
    let places = registry_from_env(
        "KIRRA_DEST_PLACES_PATH",
        PlaceRegistry::empty(),
        PlaceRegistry::from_json_str,
    )?;
    let routes = registry_from_env(
        "KIRRA_DEST_ROUTES_PATH",
        RouteRegistry::empty(),
        RouteRegistry::from_json_str,
    )?;
    let max_age_ms = env_parsed(
        "KIRRA_DEST_OBJECT_MAX_AGE_MS",
        DEFAULT_OBJECT_MAX_AGE_MS,
        |s| s.parse::<u64>().ok(),
    )?;
    let min_confidence = env_parsed(
        "KIRRA_DEST_OBJECT_MIN_CONFIDENCE",
        DEFAULT_OBJECT_MIN_CONFIDENCE,
        |s| s.parse::<f32>().ok(),
    )?;
    let standoff_m = env_parsed(
        "KIRRA_DEST_OBJECT_STANDOFF_M",
        DEFAULT_OBJECT_STANDOFF_M,
        |s| s.parse::<f64>().ok(),
    )?;
    Ok(DestinationResolver {
        places,
        routes,
        object_policy: ObjectPolicy::validated(max_age_ms, min_confidence, standoff_m)?,
    })
}

// ---------------------------------------------------------------------------
// The LLM seam — schema + prompt. The model can choose a KIND and repeat the
// operator's words; it structurally cannot emit a coordinate.
// ---------------------------------------------------------------------------

/// The constrained-decode JSON Schema for a destination choice (the
/// `destination` sibling of `kirra_planner::intent_schema`). Two string
/// fields, both required, nothing else admitted — there is NO numeric
/// property, so a schema-constrained backend cannot emit a coordinate at
/// all, and [`DestinationRef::parse_json`] refuses one after the fact anyway.
#[must_use]
pub fn destination_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "kind": {
                "type": "string",
                "enum": ["named_place", "saved_route", "tracked_object", "address"]
            },
            "query": { "type": "string" }
        },
        "required": ["kind", "query"],
        "additionalProperties": false
    })
}

/// Render the destination-choice prompt: the operator's (sanitized) request
/// plus the KNOWN place/route NAMES — names only; a coordinate never enters a
/// prompt. The model's whole job is to pick the kind and echo the operator's
/// naming of it; the trusted resolver does everything else.
#[must_use]
pub fn build_destination_prompt(
    request: &str,
    place_names: &[String],
    route_names: &[String],
) -> String {
    let request = kirra_planner::sanitize_request(request);
    let places = if place_names.is_empty() {
        "(none saved)".to_string()
    } else {
        place_names.join(", ")
    };
    let routes = if route_names.is_empty() {
        "(none saved)".to_string()
    } else {
        route_names.join(", ")
    };
    format!(
        "You classify ONE navigation request into a typed destination. You do NOT know any \
coordinates and must never invent one — a separate trusted resolver owns all geometry.\n\
\n\
Respond with ONLY one JSON object (no prose, no code fence):\n\
  {{\"kind\":\"named_place\",\"query\":\"<the place the operator named>\"}}\n\
  {{\"kind\":\"saved_route\",\"query\":\"<the route the operator named>\"}}\n\
  {{\"kind\":\"tracked_object\",\"query\":\"<the visible thing the operator named>\"}}\n\
  {{\"kind\":\"address\",\"query\":\"<the street address the operator gave>\"}}\n\
\n\
Known place names: {places}\n\
Known route names: {routes}\n\
\n\
Pick named_place or saved_route when the request names one of those; tracked_object for a \
physical thing the robot might see (a cup, a chair, a person); address for a street \
address. Put the operator's own words for the destination in query — nothing else.\n\
\n\
Request:\n  \"{request}\"\n\
\n\
Destination:"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLACES_JSON: &str = r#"{
        "version": 1,
        "map_id": "r2-lab-01",
        "places": [
            {"id": "kitchen", "name": "Kitchen", "aliases": ["the kitchen"],
             "x_m": 3.2, "y_m": -1.4, "heading_rad": 0.0},
            {"id": "charging-dock", "name": "Charging Dock",
             "aliases": ["the dock", "charger"], "x_m": 0.5, "y_m": 2.0, "heading_rad": 1.57}
        ]
    }"#;

    const ROUTES_JSON: &str = r#"{
        "version": 1,
        "map_id": "r2-lab-01",
        "routes": [
            {"id": "route-a", "name": "Route A", "aliases": ["route alpha"],
             "waypoints": [
                {"x_m": 1.0, "y_m": 0.0, "heading_rad": 0.0},
                {"x_m": 2.0, "y_m": 1.0, "heading_rad": 0.7}
             ]}
        ]
    }"#;

    fn resolver() -> DestinationResolver {
        DestinationResolver {
            places: PlaceRegistry::from_json_str(PLACES_JSON).unwrap(),
            routes: RouteRegistry::from_json_str(ROUTES_JSON).unwrap(),
            object_policy: ObjectPolicy::default(),
        }
    }

    fn no_targets() -> Vec<LabeledTarget> {
        Vec::new()
    }

    fn view<'a>(targets: &'a [LabeledTarget], stamp: Option<u64>) -> TargetView<'a> {
        TargetView {
            targets,
            stamp_ms: stamp,
        }
    }

    fn ego_origin() -> EgoView {
        EgoView {
            x_m: 0.0,
            y_m: 0.0,
            heading_rad: 0.0,
        }
    }

    fn target(label: &str, x: f64, y: f64, c: f32) -> LabeledTarget {
        LabeledTarget {
            label: label.into(),
            x_m: x,
            y_m: y,
            confidence: c,
        }
    }

    // ---- the one fail-closed parser ----------------------------------------

    #[test]
    fn parses_every_kind_and_returns_the_exact_slice() {
        for (kind, expect) in [
            ("named_place", "named_place"),
            ("saved_route", "saved_route"),
            ("tracked_object", "tracked_object"),
            ("address", "address"),
        ] {
            let raw = format!(r#"{{"kind":"{kind}","query":"the kitchen"}}"#);
            let (dref, slice) = DestinationRef::parse_json(&raw).expect(kind);
            assert_eq!(dref.kind_str(), expect);
            assert_eq!(dref.query(), "the kitchen");
            assert_eq!(slice, raw, "the slice is the exact accepted object");
        }
    }

    #[test]
    fn tolerates_llm_framing_like_the_intent_parse() {
        let (dref, _) = DestinationRef::parse_json(
            "Sure — heading there:\n```json\n{\"kind\":\"named_place\",\"query\":\"kitchen\"}\n```",
        )
        .expect("framed reply parses");
        assert_eq!(
            dref,
            DestinationRef::NamedPlace {
                query: "kitchen".into()
            }
        );
    }

    #[test]
    fn rejects_coordinates_with_the_loud_code() {
        // The core rule made mechanical: geometry in the language channel is a
        // DISTINCT refusal, whatever key spells it.
        for smuggle in [
            r#"{"kind":"named_place","query":"kitchen","x_m":3.0}"#,
            r#"{"kind":"named_place","query":"kitchen","y_m":1.0}"#,
            r#"{"kind":"address","query":"125 Main St","lat":37.0}"#,
            r#"{"kind":"address","query":"125 Main St","longitude":-122.0}"#,
            r#"{"kind":"saved_route","query":"route a","waypoints":[]}"#,
            r#"{"kind":"tracked_object","query":"cup","pose":{"x_m":1.0}}"#,
        ] {
            assert_eq!(
                DestinationRef::parse_json(smuggle).unwrap_err(),
                "DEST_COORDINATES_FORBIDDEN",
                "{smuggle}"
            );
        }
    }

    /// The voice router's small object grammar sends attributes rather than a
    /// free-text phrase; they compose into the ONE query the resolver already
    /// matches, so there is no second resolution path.
    #[test]
    fn the_tracked_object_attribute_form_composes_one_query() {
        let (dref, _) =
            DestinationRef::parse_json(r#"{"kind":"tracked_object","class":"cup","color":"red"}"#)
                .expect("attribute form parses");
        assert_eq!(
            dref,
            DestinationRef::TrackedObject {
                query: "red cup".into()
            },
            "colour + class compose in the order the detector labels them"
        );
        // Class alone is a generic request.
        assert_eq!(
            DestinationRef::from_json(r#"{"kind":"tracked_object","class":"person"}"#).unwrap(),
            DestinationRef::TrackedObject {
                query: "person".into()
            }
        );
        // And it resolves exactly like the query form against real targets.
        let r = resolver();
        let targets = [target("red cup", 3.0, 0.0, 0.9)];
        assert!(matches!(
            r.resolve(&dref, &view(&targets, Some(1_000)), ego_origin(), 1_000),
            ResolveOutcome::Resolved(_)
        ));
    }

    #[test]
    fn the_attribute_form_is_closed_and_object_only() {
        // Attributes on a kind that has no class/colour.
        for bad in [
            r#"{"kind":"named_place","class":"cup"}"#,
            r#"{"kind":"saved_route","color":"red"}"#,
            r#"{"kind":"address","class":"cup"}"#,
            // Two spellings of the same field is an ambiguity, not a merge.
            r#"{"kind":"tracked_object","query":"cup","class":"cup"}"#,
            // Nothing may ride alongside the attributes.
            r#"{"kind":"tracked_object","class":"cup","bogus":1}"#,
            // A non-string attribute.
            r#"{"kind":"tracked_object","class":7}"#,
        ] {
            assert_eq!(
                DestinationRef::parse_json(bad).unwrap_err(),
                "DEST_JSON_PARSE_ERROR",
                "{bad}"
            );
        }
        // A colour with no class names no thing at all.
        assert_eq!(
            DestinationRef::parse_json(r#"{"kind":"tracked_object","color":"red"}"#).unwrap_err(),
            "DEST_EMPTY_QUERY"
        );
        // A coordinate still outranks everything, even in the attribute form.
        assert_eq!(
            DestinationRef::parse_json(r#"{"kind":"tracked_object","class":"cup","x_m":1.0}"#)
                .unwrap_err(),
            "DEST_COORDINATES_FORBIDDEN"
        );
    }

    #[test]
    fn rejects_malformed_unknown_and_oversized() {
        assert_eq!(
            DestinationRef::parse_json("just drive somewhere").unwrap_err(),
            "DEST_JSON_PARSE_ERROR"
        );
        assert_eq!(
            DestinationRef::parse_json(r#"{"kind":"teleport","query":"moon"}"#).unwrap_err(),
            "DEST_UNKNOWN_KIND"
        );
        assert_eq!(
            DestinationRef::parse_json(r#"{"kind":"named_place","query":"  "}"#).unwrap_err(),
            "DEST_EMPTY_QUERY"
        );
        let long = "x".repeat(MAX_QUERY_CHARS + 1);
        assert_eq!(
            DestinationRef::parse_json(&format!(r#"{{"kind":"named_place","query":"{long}"}}"#))
                .unwrap_err(),
            "DEST_QUERY_TOO_LONG"
        );
        // An unknown extra field (not a coordinate) is still a strict parse error.
        assert_eq!(
            DestinationRef::parse_json(r#"{"kind":"named_place","query":"kitchen","speed":9}"#)
                .unwrap_err(),
            "DEST_JSON_PARSE_ERROR"
        );
    }

    // ---- registries: fail-closed loads -------------------------------------

    #[test]
    fn place_registry_loads_and_looks_up_by_id_name_and_alias() {
        let reg = PlaceRegistry::from_json_str(PLACES_JSON).unwrap();
        assert_eq!(reg.map_id(), "r2-lab-01");
        for q in ["kitchen", "Kitchen", "the kitchen", "THE KITCHEN!"] {
            let p = reg.lookup(q).unwrap_or_else(|| panic!("{q}"));
            assert_eq!(p.id, "kitchen");
        }
        assert_eq!(reg.lookup("charger").unwrap().id, "charging-dock");
        assert!(reg.lookup("garage").is_none());
        assert_eq!(reg.names(), vec!["Kitchen", "Charging Dock"]);
    }

    #[test]
    fn registries_refuse_every_defect() {
        // Wrong version.
        let e = PlaceRegistry::from_json_str(r#"{"version": 2, "map_id": "m", "places": []}"#)
            .unwrap_err();
        assert!(e.contains("version"), "{e}");
        // Missing map_id content.
        let e = PlaceRegistry::from_json_str(r#"{"version": 1, "map_id": "  ", "places": []}"#)
            .unwrap_err();
        assert!(e.contains("map_id"), "{e}");
        // An overflowing coordinate (1e999) is refused — serde_json rejects
        // it at parse ("number out of range") before the belt-and-suspenders
        // finiteness check even runs. Either way: fail-closed, never inf.
        let e = PlaceRegistry::from_json_str(
            r#"{"version": 1, "map_id": "m", "places":
               [{"id":"a","name":"A","x_m":1e999,"y_m":0.0}]}"#,
        )
        .unwrap_err();
        assert!(
            e.contains("out of range") || e.contains("non-finite"),
            "{e}"
        );
        // Duplicate key across entries (name of one == alias of another).
        let e = PlaceRegistry::from_json_str(
            r#"{"version": 1, "map_id": "m", "places": [
                {"id":"a","name":"Dock","x_m":1.0,"y_m":0.0},
                {"id":"b","name":"B","aliases":["dock"],"x_m":2.0,"y_m":0.0}
            ]}"#,
        )
        .unwrap_err();
        assert!(e.contains("collides"), "{e}");
        // Two DISTINCT entries sharing the same id collide (uniqueness is by
        // entry position — the parity the Python CLI mirrors).
        let e = PlaceRegistry::from_json_str(
            r#"{"version": 1, "map_id": "m", "places": [
                {"id":"kitchen","name":"Kitchen","x_m":1.0,"y_m":0.0},
                {"id":"kitchen","name":"Kitchen Two","x_m":2.0,"y_m":0.0}
            ]}"#,
        )
        .unwrap_err();
        assert!(e.contains("collides"), "{e}");
        // Unknown field — a typo'd registry must not half-load.
        let e = PlaceRegistry::from_json_str(
            r#"{"version": 1, "map_id": "m", "places":
               [{"id":"a","name":"A","x_m":1.0,"y_m":0.0,"z_m":1.0}]}"#,
        )
        .unwrap_err();
        assert!(e.contains("z_m") || e.contains("unknown"), "{e}");
        // Route with no waypoints.
        let e = RouteRegistry::from_json_str(
            r#"{"version": 1, "map_id": "m", "routes":
               [{"id":"r","name":"R","waypoints":[]}]}"#,
        )
        .unwrap_err();
        assert!(e.contains("waypoints"), "{e}");
    }

    // ---- resolver outcomes -------------------------------------------------

    #[test]
    fn named_place_resolves_to_the_registry_pose() {
        let r = resolver();
        let out = r.resolve(
            &DestinationRef::NamedPlace {
                query: "the kitchen".into(),
            },
            &view(&no_targets(), None),
            ego_origin(),
            5_000,
        );
        let ResolveOutcome::Resolved(g) = out else {
            panic!("must resolve, got {out:?}")
        };
        assert_eq!(g.destination_id, "kitchen");
        assert_eq!(g.map_id.as_deref(), Some("r2-lab-01"));
        assert_eq!(g.source, DestinationSource::PlaceRegistry);
        assert_eq!(g.resolved_at_ms, 5_000);
        assert!((g.goal_pose.x_m - 3.2).abs() < 1e-12);
        assert!((g.goal_pose.y_m - (-1.4)).abs() < 1e-12);
        assert!(g.route_waypoints.is_empty());
    }

    #[test]
    fn saved_route_resolves_first_waypoint_plus_the_full_list() {
        let r = resolver();
        let out = r.resolve(
            &DestinationRef::SavedRoute {
                query: "route alpha".into(),
            },
            &view(&no_targets(), None),
            ego_origin(),
            7_000,
        );
        let ResolveOutcome::Resolved(g) = out else {
            panic!("must resolve, got {out:?}")
        };
        assert_eq!(g.destination_id, "route-a");
        assert_eq!(g.source, DestinationSource::RouteRegistry);
        assert_eq!(g.route_waypoints.len(), 2);
        assert!(
            (g.goal_pose.x_m - 1.0).abs() < 1e-12,
            "first waypoint is the goal"
        );
    }

    #[test]
    fn unknown_and_unregistered_fail_closed_with_distinct_codes() {
        let r = resolver();
        let miss = r.resolve(
            &DestinationRef::NamedPlace {
                query: "garage".into(),
            },
            &view(&no_targets(), None),
            ego_origin(),
            0,
        );
        assert_eq!(miss.code(), "DEST_NOT_FOUND");
        assert!(!miss.sentence("the garage").is_empty());

        let empty = DestinationResolver::default();
        let none = empty.resolve(
            &DestinationRef::NamedPlace {
                query: "kitchen".into(),
            },
            &view(&no_targets(), None),
            ego_origin(),
            0,
        );
        assert_eq!(none.code(), "DEST_NO_REGISTRY");
    }

    #[test]
    fn address_is_a_typed_unsupported_seam_never_a_fake_coordinate() {
        let r = resolver();
        let out = r.resolve(
            &DestinationRef::Address {
                query: "125 Main Street".into(),
            },
            &view(&no_targets(), None),
            ego_origin(),
            0,
        );
        assert_eq!(
            out,
            ResolveOutcome::Unsupported {
                code: "DEST_UNSUPPORTED_ADDRESS"
            }
        );
        assert_eq!(out.code(), "DEST_UNSUPPORTED_ADDRESS");
        assert!(out.sentence("125 Main Street").contains("addresses"));
    }

    #[test]
    fn tracked_object_resolves_with_a_standoff_short_of_the_object() {
        let r = resolver();
        let targets = [target("red cup", 4.0, 0.0, 0.9)];
        let out = r.resolve(
            &DestinationRef::TrackedObject {
                query: "red cup".into(),
            },
            &view(&targets, Some(1_000)),
            ego_origin(),
            1_000,
        );
        let ResolveOutcome::Resolved(g) = out else {
            panic!("must resolve, got {out:?}")
        };
        assert_eq!(g.source, DestinationSource::ObjectTrack);
        assert_eq!(g.map_id, None, "a live sighting has no map identity");
        // 4.0 m ahead, 0.75 m standoff → goal 3.25 m ahead, facing the cup.
        assert!((g.goal_pose.x_m - 3.25).abs() < 1e-9, "{}", g.goal_pose.x_m);
        assert!(g.goal_pose.y_m.abs() < 1e-9);
        assert!(g.goal_pose.heading_rad.abs() < 1e-9);
    }

    #[test]
    fn tracked_object_within_the_standoff_grounds_at_the_ego() {
        let r = resolver();
        let targets = [target("cup", 0.5, 0.0, 0.9)];
        let out = r.resolve(
            &DestinationRef::TrackedObject {
                query: "cup".into(),
            },
            &view(&targets, Some(1_000)),
            EgoView {
                x_m: 10.0,
                y_m: 5.0,
                heading_rad: 0.0,
            },
            1_000,
        );
        let ResolveOutcome::Resolved(g) = out else {
            panic!("{out:?}")
        };
        assert!((g.goal_pose.x_m - 10.0).abs() < 1e-9 && (g.goal_pose.y_m - 5.0).abs() < 1e-9);
    }

    #[test]
    fn tracked_object_grounds_through_the_ego_pose_into_the_world_frame() {
        // Ego at (10, 5) facing +90°: a cup 2 m ahead is 2 m north; a 0.75 m
        // standoff puts the goal at (10, 6.25), heading north.
        let r = resolver();
        let targets = [target("cup", 2.0, 0.0, 0.9)];
        let out = r.resolve(
            &DestinationRef::TrackedObject {
                query: "cup".into(),
            },
            &view(&targets, Some(1_000)),
            EgoView {
                x_m: 10.0,
                y_m: 5.0,
                heading_rad: std::f64::consts::FRAC_PI_2,
            },
            1_000,
        );
        let ResolveOutcome::Resolved(g) = out else {
            panic!("{out:?}")
        };
        assert!((g.goal_pose.x_m - 10.0).abs() < 1e-9, "{}", g.goal_pose.x_m);
        assert!((g.goal_pose.y_m - 6.25).abs() < 1e-9, "{}", g.goal_pose.y_m);
        assert!((g.goal_pose.heading_rad - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
    }

    #[test]
    fn tracked_object_refusals_map_to_the_explicit_outcomes() {
        let r = resolver();
        let ego = ego_origin();
        // Stale: no frame at all.
        let targets = [target("cup", 2.0, 0.0, 0.9)];
        assert_eq!(
            r.resolve(
                &DestinationRef::TrackedObject {
                    query: "cup".into()
                },
                &view(&targets, None),
                ego,
                1_000
            ),
            ResolveOutcome::Stale
        );
        // Stale: frame older than the 2 s policy budget.
        assert_eq!(
            r.resolve(
                &DestinationRef::TrackedObject {
                    query: "cup".into()
                },
                &view(&targets, Some(1_000)),
                ego,
                4_000
            ),
            ResolveOutcome::Stale
        );
        // Fresh at exactly the policy edge still resolves (2 s inclusive).
        assert!(matches!(
            r.resolve(
                &DestinationRef::TrackedObject {
                    query: "cup".into()
                },
                &view(&targets, Some(1_000)),
                ego,
                3_000
            ),
            ResolveOutcome::Resolved(_)
        ));
        // Not seen.
        let chairs = [target("chair", 2.0, 0.0, 0.9)];
        assert_eq!(
            r.resolve(
                &DestinationRef::TrackedObject {
                    query: "cup".into()
                },
                &view(&chairs, Some(1_000)),
                ego,
                1_000
            ),
            ResolveOutcome::NotFound {
                code: "DEST_NOT_SEEN"
            }
        );
        // Low confidence: 0.5 clears the object_goal channel default (0.40)
        // but NOT this policy's 0.70 floor — the destination policy binds.
        let dim = [target("cup", 2.0, 0.0, 0.5)];
        assert_eq!(
            r.resolve(
                &DestinationRef::TrackedObject {
                    query: "cup".into()
                },
                &view(&dim, Some(1_000)),
                ego,
                1_000
            ),
            ResolveOutcome::NotFound {
                code: "DEST_LOW_CONFIDENCE"
            }
        );
        // Ambiguous tie.
        let two = [target("cup", 2.0, 0.0, 0.9), target("cup", 2.05, 0.1, 0.9)];
        assert!(matches!(
            r.resolve(
                &DestinationRef::TrackedObject {
                    query: "cup".into()
                },
                &view(&two, Some(1_000)),
                ego,
                1_000
            ),
            ResolveOutcome::Ambiguous { .. }
        ));
        // Behind the ego.
        let behind = [target("cup", -2.0, 0.0, 0.9)];
        assert_eq!(
            r.resolve(
                &DestinationRef::TrackedObject {
                    query: "cup".into()
                },
                &view(&behind, Some(1_000)),
                ego,
                1_000
            ),
            ResolveOutcome::NotFound {
                code: "DEST_BEHIND_EGO"
            }
        );
    }

    #[test]
    fn object_policy_is_bounds_checked() {
        assert!(ObjectPolicy::validated(2_000, 0.7, 0.75).is_ok());
        assert!(ObjectPolicy::validated(0, 0.7, 0.75).is_err());
        assert!(ObjectPolicy::validated(2_000, 1.5, 0.75).is_err());
        assert!(ObjectPolicy::validated(2_000, f32::NAN, 0.75).is_err());
        assert!(ObjectPolicy::validated(2_000, 0.7, -0.1).is_err());
        assert!(ObjectPolicy::validated(2_000, 0.7, 9.0).is_err());
        assert!(ObjectPolicy::validated(2_000, 0.7, f64::INFINITY).is_err());
    }

    // ---- outcome codes + narration -----------------------------------------

    #[test]
    fn every_refusal_outcome_has_a_stable_code_and_a_sentence() {
        let outcomes = [
            ResolveOutcome::Ambiguous { candidates: vec![] },
            ResolveOutcome::Ambiguous {
                candidates: vec!["kitchen".into(), "kitchenette".into()],
            },
            ResolveOutcome::NotFound {
                code: "DEST_NOT_FOUND",
            },
            ResolveOutcome::NotFound {
                code: "DEST_NO_REGISTRY",
            },
            ResolveOutcome::NotFound {
                code: "DEST_NOT_SEEN",
            },
            ResolveOutcome::NotFound {
                code: "DEST_LOW_CONFIDENCE",
            },
            ResolveOutcome::NotFound {
                code: "DEST_BEHIND_EGO",
            },
            ResolveOutcome::NotFound {
                code: "DEST_NON_FINITE",
            },
            ResolveOutcome::Stale,
            ResolveOutcome::Unsupported {
                code: "DEST_UNSUPPORTED_ADDRESS",
            },
        ];
        for o in &outcomes {
            assert!(o.code().starts_with("DEST_"), "{o:?}");
            let s = o.sentence("the kitchen");
            assert!(!s.is_empty() && s.ends_with(['.', '?']), "{o:?} -> {s:?}");
        }
    }

    // ---- the LLM seam: schema + prompt -------------------------------------

    /// The lockstep pin: every kind the schema constrains the model to must be
    /// one the fail-closed parser accepts, and the schema must carry NO
    /// numeric property — the structural "no coordinates" guarantee.
    #[test]
    fn destination_schema_kinds_match_the_parser_and_carry_no_numbers() {
        let s = destination_schema();
        assert_eq!(s["required"], serde_json::json!(["kind", "query"]));
        assert_eq!(s["additionalProperties"], serde_json::json!(false));
        let kinds: Vec<String> = s["properties"]["kind"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            kinds,
            ["named_place", "saved_route", "tracked_object", "address"]
        );
        for kind in &kinds {
            let raw = format!(r#"{{"kind":"{kind}","query":"somewhere"}}"#);
            assert!(
                DestinationRef::parse_json(&raw).is_ok(),
                "parser must accept schema kind {kind}"
            );
        }
        // No property admits a number — the model cannot emit a coordinate.
        for (name, prop) in s["properties"].as_object().unwrap() {
            assert_eq!(
                prop["type"], "string",
                "property {name} must be a string — no numeric channel"
            );
        }
    }

    #[test]
    fn destination_prompt_carries_names_but_never_coordinates() {
        let r = resolver();
        let p = build_destination_prompt(
            "take me to the kitchen",
            &r.places.names(),
            &r.routes.names(),
        );
        assert!(p.contains("Kitchen") && p.contains("Route A"));
        assert!(p.contains("take me to the kitchen"));
        assert!(
            p.contains("never invent"),
            "the no-coordinates rule is stated"
        );
        // The registry coordinates must not leak into the prompt.
        for leak in ["3.2", "-1.4", "1.57", "x_m", "y_m"] {
            assert!(!p.contains(leak), "coordinate leak {leak:?} in prompt");
        }
    }
}
