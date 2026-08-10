//! **Tier 3 box 3c — one coherent point across several projections.**
//!
//! An answer that composes projections must read them at ONE coherent point, or
//! report each coordinate explicitly. This module provides the first arm, and
//! carries the second alongside it.
//!
//! # Why this exists as a type rather than as discipline
//!
//! Identity, claims and subject summaries are three independently-folded
//! projections. Each advances its own checkpoint row when its own fold commits,
//! and nothing coordinates them. A composed read that calls
//! [`WorldStore::current`] and then [`WorldStore::identity_view`] can therefore
//! observe a fold landing between the two calls and answer from two different
//! states of the world — the exact failure [`IdentityView`]'s own docs rule out
//! *within* a walk, reappearing *between* walks.
//!
//! [`IdentityView`]: crate::entity_projection::IdentityView
//!
//! # The mechanism, and why it is a guarantee rather than a detector
//!
//! Every projection is a table in ONE SQLite database, and every fold commits
//! atomically. So a single read transaction — which in WAL mode holds one
//! snapshot of the whole database for its lifetime — sees each projection at a
//! committed fold boundary, and sees *the same set of commits* for all of them.
//! That is coherence by construction, not drift detection after the fact.
//!
//! This matters for what the box is allowed to claim. Detecting drift and
//! refusing would also satisfy 3c's fallback arm, but it is strictly weaker: it
//! turns a concurrent fold into a refused answer, where a snapshot turns it into
//! a correct one.
//!
//! # What this is NOT
//!
//! It is **not** the generation-pinned read that `KIRRA-WM-ANSWER-IDENTITY-001`
//! needs, and [`SnapshotCoordinate`] is **not** an `AnswerRef`. A snapshot is
//! coherent for as long as it is held and cannot be re-entered once dropped;
//! re-executing a query against a *recorded* coordinate needs a way to read
//! `world_current` as of a generation, which does not exist. That gap has its
//! own open box in `docs/design/WM_SCOPE.md`, and
//! `KIRRA-WM-ANSWERREF-NAMING-001` reserves the name `AnswerRef` for the day it
//! closes. Naming this type `AnswerRef` would put the ruled guarantee's name on
//! a mechanism that cannot honour it.

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};

use crate::entity_projection::{self, IdentityView, ProjectedEntity};
use crate::projection::{self, ProjectedClaim};
use crate::subject_projection;
use crate::{claim_from_row, StoreError, CLAIM_COLUMNS};

/// Where ONE projection stood when a snapshot observed it.
///
/// Carries the `state_digest` as well as the generation because a generation
/// alone cannot distinguish a fold that advanced from a rebuild that landed on
/// the same head — the pair is what the fold itself commits, so it is what an
/// observer should record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionCoordinate {
    name: &'static str,
    generation: i64,
    state_digest: String,
}

impl ProjectionCoordinate {
    /// The projection's checkpoint name, as the fold writes it.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// How far this projection's fold has consumed.
    ///
    /// **Not comparable to another projection's generation.** See
    /// [`SnapshotCoordinate`] for why that comparison is meaningless rather than
    /// merely unreliable.
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// The digest of the state the fold left, or `""` if never folded.
    pub fn state_digest(&self) -> &str {
        &self.state_digest
    }

    /// Whether this projection has ever been folded.
    ///
    /// A projection whose table has not been installed reports generation 0,
    /// matching [`WorldStore::projection_generation`]'s existing convention —
    /// so "absent" and "folded nothing" are deliberately the same observation.
    /// They have the same consequence for a reader: there is nothing there.
    pub fn is_unfolded(&self) -> bool {
        self.generation == 0
    }
}

/// Where EVERY projection stood, observed at one coherent point.
///
/// # These numbers are not comparable to each other
///
/// The obvious check — assert two projections sit at the same generation — is
/// wrong, and wrong in the direction that looks like rigour. `world_current`
/// and `subject_summary` advance their checkpoint past every event *considered*,
/// including events they adopt nothing from; the entity fold advances only to
/// the generation of the last *adjudication* it folded. So appending any
/// non-adjudication event leaves the entity checkpoint legitimately behind the
/// other two, with all three fully folded and nothing wrong.
///
/// An equality check would therefore report constant false drift, and the fix
/// someone reaches for under deadline is to delete the check. Coherence here
/// comes from the snapshot, not from comparing these numbers; they are recorded
/// so an answer can say *which* state it came from, which is 3c's second arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCoordinate {
    world_current: ProjectionCoordinate,
    entities: ProjectionCoordinate,
    subject_summary: ProjectionCoordinate,
}

impl SnapshotCoordinate {
    /// The claims projection's coordinate.
    pub fn world_current(&self) -> &ProjectionCoordinate {
        &self.world_current
    }

    /// The identity projection's coordinate.
    pub fn entities(&self) -> &ProjectionCoordinate {
        &self.entities
    }

    /// The subject-summary projection's coordinate.
    pub fn subject_summary(&self) -> &ProjectionCoordinate {
        &self.subject_summary
    }

    /// Every coordinate, in a stable order.
    ///
    /// All three are recorded even when a read touched one, deliberately: a
    /// coordinate that listed only the projections a particular read happened to
    /// consult would be a record that changes shape with the query, and a reader
    /// comparing two of them could not tell "this projection was not read" from
    /// "this projection did not exist".
    pub fn all(&self) -> [&ProjectionCoordinate; 3] {
        [&self.world_current, &self.entities, &self.subject_summary]
    }
}

/// **A coherent multi-projection read.**
///
/// Holds one SQLite read transaction for its lifetime, so every read taken
/// through it observes one state of the database — and therefore one state of
/// every projection in it.
///
/// # The snapshot begins at the first read, not at construction
///
/// The transaction is DEFERRED, so SQLite establishes the snapshot when the
/// first statement runs. This does not weaken coherence — every read still
/// agrees with every other — but it does mean a snapshot held open and unused
/// is not "as of" the moment it was created. Stated because the alternative
/// reading is the one that produces a stale-data bug report years later.
///
/// # Holding one open is not free
///
/// A read transaction holds a WAL read-mark, which prevents checkpointing from
/// reclaiming WAL frames beyond it. Take one for a composed read and drop it;
/// do not park one across a request loop.
pub struct ReadSnapshot<'a> {
    tx: rusqlite::Transaction<'a>,
}

impl<'a> ReadSnapshot<'a> {
    pub(crate) fn new(tx: rusqlite::Transaction<'a>) -> Self {
        Self { tx }
    }

    /// The current claims for `subject`, holding at `now_ms`.
    ///
    /// Identical in result to [`WorldStore::current`] — the same code answers
    /// both — except that it is bound to this snapshot.
    ///
    /// [`WorldStore::current`]: crate::WorldStore::current
    pub fn current(&self, subject: &str, now_ms: i64) -> Result<Vec<ProjectedClaim>, StoreError> {
        current_on(&self.tx, subject, now_ms)
    }

    /// The identity graph, loaded from this snapshot.
    ///
    /// Unlike [`WorldStore::identity_view`] before 3c, the rows and the
    /// generation label are read from ONE state: the view cannot be stamped
    /// with a generation newer than the rows it holds.
    ///
    /// [`WorldStore::identity_view`]: crate::WorldStore::identity_view
    pub fn identity_view(&self) -> Result<IdentityView, StoreError> {
        Ok(IdentityView::new(
            load_entity_projection_on(&self.tx)?,
            checkpoint_on(&self.tx, entity_projection::ENTITY_PROJECTION)?.0,
        ))
    }

    /// Where every projection stood in this snapshot.
    pub fn coordinate(&self) -> Result<SnapshotCoordinate, StoreError> {
        coordinate_on(&self.tx)
    }

    /// **Has the identity graph consumed every adjudication the log holds?**
    ///
    /// The question a composed read must ask before trusting a resolution, and
    /// it is deliberately *not* "has the entity projection been folded".
    ///
    /// Those differ in both directions, which is why the cheap check is the
    /// wrong one:
    ///
    /// - A store that has **never** folded the entity projection because it has
    ///   **no adjudications at all** is perfectly current. There are no merges
    ///   to miss, so an object stands for itself. Treating that as unresolvable
    ///   would refuse every object-bearing claim on the overwhelmingly common
    ///   store — an availability failure bought for no safety.
    /// - A store folded once, with merges recorded **since**, has a projection
    ///   that exists and is *stale*. A folded-or-not check calls that fine and
    ///   resolves against data known to be out of date — the dangerous case,
    ///   waved through.
    ///
    /// Answered from the same snapshot as everything else, so a fold committing
    /// concurrently cannot make this answer disagree with the view it describes.
    pub fn identity_is_current(&self) -> Result<bool, StoreError> {
        if !table_exists(&self.tx, "world_events")? {
            return Ok(true);
        }
        let checkpoint = checkpoint_on(&self.tx, entity_projection::ENTITY_PROJECTION)?.0;
        let pending: Option<i64> = self
            .tx
            .query_row(
                "SELECT 1 FROM world_events
                 WHERE kind = ?1 AND claim_status = 'confirmed' AND generation > ?2
                 LIMIT 1",
                params![crate::adjudication_record::ADJUDICATION_KIND, checkpoint],
                |r| r.get(0),
            )
            .optional()?;
        Ok(pending.is_none())
    }
}

/// Read one projection's checkpoint row: `(generation, state_digest)`.
///
/// A missing checkpoint table or a missing row is `(0, "")` rather than an
/// error, matching the existing generation readers: a projection that has never
/// folded has consumed nothing, which is a fact and not a fault.
pub(crate) fn checkpoint_on(conn: &Connection, name: &str) -> Result<(i64, String), StoreError> {
    let installed: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='projection_checkpoint'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if installed.is_none() {
        return Ok((0, String::new()));
    }
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT generation, state_digest FROM projection_checkpoint WHERE name = ?1",
            params![name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.unwrap_or((0, String::new())))
}

pub(crate) fn coordinate_on(conn: &Connection) -> Result<SnapshotCoordinate, StoreError> {
    let one = |name: &'static str| -> Result<ProjectionCoordinate, StoreError> {
        let (generation, state_digest) = checkpoint_on(conn, name)?;
        Ok(ProjectionCoordinate {
            name,
            generation,
            state_digest,
        })
    };
    Ok(SnapshotCoordinate {
        world_current: one(projection::CURRENT_PROJECTION)?,
        entities: one(entity_projection::ENTITY_PROJECTION)?,
        subject_summary: one(subject_projection::SUBJECT_SUMMARY_PROJECTION)?,
    })
}

/// The body of `WorldStore::current`, over any connection or transaction.
///
/// Extracted rather than duplicated so a snapshot read and a direct read cannot
/// drift apart: two copies of a `holds_at` filter is exactly the kind of
/// divergence that shows up as "the composed answer disagrees with the simple
/// one" long after the copy was made.
pub(crate) fn current_on(
    conn: &Connection,
    subject: &str,
    now_ms: i64,
) -> Result<Vec<ProjectedClaim>, StoreError> {
    if !table_exists(conn, "world_current")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {CLAIM_COLUMNS} FROM world_current WHERE subject = ?1 ORDER BY predicate_key"
    ))?;
    let rows = stmt.query_map(params![subject], claim_from_row)?;
    let mut out = Vec::new();
    for c in rows {
        let c = c?;
        if c.holds_at(now_ms) {
            out.push(c);
        }
    }
    Ok(out)
}

/// The body of `WorldStore::load_entity_projection`, over any connection.
pub(crate) fn load_entity_projection_on(
    conn: &Connection,
) -> Result<BTreeMap<String, ProjectedEntity>, StoreError> {
    if !table_exists(conn, "entities_projection")? {
        return Ok(BTreeMap::new());
    }
    let mut stmt = conn.prepare(
        "SELECT entity_id, lifecycle, redirect, origin, contradicted, contradiction
         FROM entities_projection ORDER BY entity_id ASC",
    )?;
    let mut out = BTreeMap::new();
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let id: String = r.get(0)?;
        let token: String = r.get(1)?;
        let redirect: Option<String> = r.get(2)?;
        let origin: Option<String> = r.get(3)?;
        let contradicted: i64 = r.get(4)?;
        let detail: Option<String> = r.get(5)?;
        let contradiction = match (contradicted != 0, detail.as_deref()) {
            (false, _) => None,
            (true, Some(raw)) => Some(entity_projection::contradiction_from_json(raw).map_err(
                |e| StoreError::CorruptEntityProjectionRow {
                    detail: format!("{id}: {e}"),
                },
            )?),
            // Flagged contradicted with nothing recorded: the fold always
            // writes both, so this is a row edited underneath the store.
            (true, None) => {
                return Err(StoreError::CorruptEntityProjectionRow {
                    detail: format!("{id}: contradicted with no contradiction recorded"),
                })
            }
        };
        let lifecycle = entity_projection::lifecycle_from_columns(
            &token,
            redirect.as_deref(),
            origin.as_deref(),
        )
        .map_err(|e| StoreError::CorruptEntityProjectionRow {
            detail: format!("{id}: {e}"),
        })?;
        let entity = kirra_world::reference::EntityId::new(&id).map_err(|e| {
            StoreError::CorruptEntityProjectionRow {
                detail: format!("{id}: inadmissible entity id: {e:?}"),
            }
        })?;
        out.insert(
            id,
            ProjectedEntity {
                entity,
                lifecycle,
                contradiction,
            },
        );
    }
    Ok(out)
}

pub(crate) fn table_exists(conn: &Connection, name: &str) -> Result<bool, StoreError> {
    let n: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?1",
            params![name],
            |r| r.get(0),
        )
        .optional()?;
    Ok(n.is_some())
}
