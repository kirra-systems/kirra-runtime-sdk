// crates/kirra-persistence/src/schema_spec.rs
//
// #1033 (P-Schema) — ONE declarative source of truth for the columns of every
// table that exists in BOTH backends: the SQLite backend realized here in
// `VerifierStore::new` (+ `init_audit_chain_schema`) and the Postgres backend
// realized in `kirra-verifier-pg`'s `BASELINE_DDL` / `PG_MIGRATIONS`.
//
// THE DRIFT PROBLEM this closes: the two backends share the migration *engine*
// (`run_migrations_generic`) and a behavioural conformance suite, but their
// column DDL was hand-authored TWICE with nothing asserting the two agree. A
// column added / a `NOT NULL` flipped / a type changed on one backend had no
// compiler- or CI-enforced obligation to mirror on the other; drift was caught
// only if a behavioural `assert_*_store_contract` happened to exercise that exact
// path AND the `postgres-conformance` lane ran.
//
// THE FIX: `SHARED_TABLES` below pins, per shared table, every column's
// (name, logical type, nullability, primary-key membership). BOTH backends
// introspect their OWN live schema and diff it against this ONE spec via the
// SAME `diff_table` comparator:
//   * SQLite — `assert_sqlite_conforms` (this crate's test) reads `PRAGMA
//     table_info` and runs the diff in `Dialect::Sqlite`.
//   * Postgres — `kirra-verifier-pg`'s `schema_matches_shared_spec` test reads
//     `information_schema.columns` (+ the PK set) and runs the diff in
//     `Dialect::Postgres`.
// Because each backend must equal the spec, they must equal each other: a
// one-sided DDL change reds one of the two conformance tests until the spec AND
// the other backend are brought into agreement. The diff is BIDIRECTIONAL — an
// un-spec'd live column fails too, so a column added to one backend cannot slip
// through unnoticed.
//
// SCOPE: only the 13 tables present in BOTH backends. SQLite-only tables (the
// audit-chain ledger, dependency graph, posture-event log, clearance grants,
// fabric causal log, …) are NOT part of the storage-portability contract and are
// intentionally absent. `UNIQUE` / `CHECK` / `DEFAULT` are out of scope (they do
// not change a column's presence, type, or nullability); the benign
// type-CATEGORY differences the two dialects legitimately use — SQLite `INTEGER`
// vs PG `bigint`/`integer`, SQLite `REAL` vs PG `double precision`, SQLite
// `INTEGER` 0/1 vs PG native `boolean` — are absorbed by `LogicalType`, which is
// what makes a cross-dialect spec possible at all.

use rusqlite::Connection;

/// A backend-independent column type. Each `LogicalType` maps to a SET of
/// concrete declared types per dialect, so the spec pins the SEMANTIC type while
/// tolerating the dialect-idiomatic spelling (the benign category differences the
/// review explicitly calls acceptable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicalType {
    /// Whole number. SQLite `INTEGER`; PG `bigint` / `integer` / `smallint`.
    Integer,
    /// UTF-8 string. SQLite `TEXT`; PG `text` / `varchar`.
    Text,
    /// Floating point. SQLite `REAL`; PG `double precision` / `real` / `numeric`.
    Real,
    /// Boolean. SQLite has no bool → `INTEGER` (0/1); PG native `boolean`.
    Bool,
}

impl LogicalType {
    /// Does a SQLite `PRAGMA table_info.type` string satisfy this logical type?
    /// (Case-insensitive; SQLite returns the DECLARED type verbatim.)
    pub fn matches_sqlite(self, declared: &str) -> bool {
        let d = declared.trim().to_ascii_uppercase();
        match self {
            // SQLite stores a bool as an INTEGER 0/1 — indistinguishable from a
            // plain integer at the storage layer. The bool/int DISTINCTION is
            // enforced on the PG side (bigint vs boolean); here both accept
            // INTEGER, which is faithful to what SQLite can actually express.
            LogicalType::Integer | LogicalType::Bool => d == "INTEGER",
            LogicalType::Text => d == "TEXT",
            LogicalType::Real => d == "REAL",
        }
    }

    /// Does a Postgres `information_schema.columns.data_type` string satisfy this
    /// logical type? (Case-insensitive; PG returns a canonical lowercase name.)
    pub fn matches_pg(self, data_type: &str) -> bool {
        let d = data_type.trim().to_ascii_lowercase();
        match self {
            LogicalType::Integer => matches!(d.as_str(), "bigint" | "integer" | "smallint"),
            LogicalType::Text => matches!(d.as_str(), "text" | "character varying" | "varchar"),
            LogicalType::Real => {
                matches!(d.as_str(), "double precision" | "real" | "numeric")
            }
            LogicalType::Bool => d == "boolean",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LogicalType::Integer => "Integer",
            LogicalType::Text => "Text",
            LogicalType::Real => "Real",
            LogicalType::Bool => "Bool",
        }
    }
}

/// One column in the shared spec.
#[derive(Debug, Clone, Copy)]
pub struct SharedColumn {
    pub name: &'static str,
    pub ty: LogicalType,
    /// Whether the column admits NULL. A PRIMARY KEY column is always treated as
    /// NOT NULL (see `LiveColumn::effective_not_null`) regardless of what a
    /// dialect reports per-column — this normalizes SQLite's well-known quirk
    /// that a `TEXT PRIMARY KEY` column reports `notnull = 0`, versus PG where a
    /// PK column is always `is_nullable = NO`.
    pub nullable: bool,
    pub primary_key: bool,
}

/// One table in the shared spec.
#[derive(Debug, Clone, Copy)]
pub struct SharedTable {
    pub name: &'static str,
    pub columns: &'static [SharedColumn],
}

// Terse const constructors so the table literals read like the DDL they pin.
const fn pk(name: &'static str, ty: LogicalType) -> SharedColumn {
    SharedColumn {
        name,
        ty,
        nullable: false,
        primary_key: true,
    }
}
const fn nn(name: &'static str, ty: LogicalType) -> SharedColumn {
    SharedColumn {
        name,
        ty,
        nullable: false,
        primary_key: false,
    }
}
const fn null(name: &'static str, ty: LogicalType) -> SharedColumn {
    SharedColumn {
        name,
        ty,
        nullable: true,
        primary_key: false,
    }
}

use LogicalType::{Bool, Integer, Real, Text};

const HA_STATE: &[SharedColumn] = &[
    pk("id", Integer),
    nn("epoch", Integer),
    null("active_instance_id", Text),
    nn("updated_at_ms", Integer),
];

const NODES: &[SharedColumn] = &[
    pk("node_id", Text),
    nn("status_json", Text),
    nn("registered_at_ms", Integer),
    nn("last_trust_update_ms", Integer),
    null("ak_public_pem", Text),
    null("expected_pcr16_digest_hex", Text),
    null("site", Text),
    null("firmware_version", Text),
];

const POSTURE_ENGINE_STATE: &[SharedColumn] = &[pk("key", Text), nn("value", Text)];

const TRUSTED_FEDERATION_CONTROLLERS: &[SharedColumn] = &[
    pk("controller_id", Text),
    nn("public_key_b64", Text),
    nn("registered_at_ms", Integer),
];

const FEDERATION_REPORT_NONCES: &[SharedColumn] = &[
    pk("nonce_hex", Text),
    nn("source_controller_id", Text),
    nn("seen_at_ms", Integer),
];

const INDUSTRIAL_MESSAGE_SEQ: &[SharedColumn] = &[
    pk("source_id", Text),
    nn("last_sequence", Integer),
    nn("last_seen_ms", Integer),
];

const OPERATORS: &[SharedColumn] = &[
    pk("operator_id", Text),
    nn("pubkey_pem", Text),
    nn("registered_at_ms", Integer),
    null("revoked_at_ms", Integer),
];

const API_PRINCIPALS: &[SharedColumn] = &[
    pk("principal_id", Text),
    nn("token_sha256", Text),
    nn("role", Text),
    nn("created_at_ms", Integer),
    null("revoked_at_ms", Integer),
];

const CERT_PRINCIPALS: &[SharedColumn] = &[
    pk("principal_id", Text),
    nn("cert_sha256", Text),
    nn("role", Text),
    nn("created_at_ms", Integer),
    null("revoked_at_ms", Integer),
    null("not_after_ms", Integer),
];

const FABRIC_ASSETS: &[SharedColumn] = &[
    pk("asset_id", Text),
    nn("asset_type", Text),
    nn("display_name", Text),
    nn("kinematic_profile", Text),
    nn("registered_at_ms", Integer),
    nn("last_seen_ms", Integer),
    nn("metadata_json", Text),
];

const OTA_CAMPAIGNS: &[SharedColumn] = &[
    pk("campaign_id", Text),
    nn("artifact_digest", Text),
    nn("artifact_version", Text),
    nn("cohorts_json", Text),
    nn("stages_json", Text),
    nn("stage_index", Integer),
    nn("rollout_percent", Integer),
    nn("state", Text),
    null("halt_reason", Text),
    nn("created_at_ms", Integer),
    nn("updated_at_ms", Integer),
    null("artifact_signature_b64", Text),
    null("uptane_metadata_json", Text),
];

const NODE_ARTIFACT_STATUS: &[SharedColumn] = &[
    pk("node_id", Text),
    nn("applied_digest", Text),
    null("campaign_id", Text),
    null("artifact_version", Text),
    nn("reported_at_ms", Integer),
    nn("attested", Bool),
];

const AV_SUBSYSTEM_META: &[SharedColumn] = &[
    pk("node_id", Text),
    nn("subsystem_type", Text),
    nn("hardware_id", Text),
    nn("confidence_floor", Real),
    nn("last_telemetry_ms", Integer),
    nn("recovery_streak_count", Integer),
    nn("recovery_streak_start_ms", Integer),
];

/// The single source of truth: every table present in BOTH backends, with the
/// columns both must realize identically (modulo the dialect type-category
/// tolerances baked into `LogicalType`).
pub const SHARED_TABLES: &[SharedTable] = &[
    SharedTable {
        name: "ha_state",
        columns: HA_STATE,
    },
    SharedTable {
        name: "nodes",
        columns: NODES,
    },
    SharedTable {
        name: "posture_engine_state",
        columns: POSTURE_ENGINE_STATE,
    },
    SharedTable {
        name: "trusted_federation_controllers",
        columns: TRUSTED_FEDERATION_CONTROLLERS,
    },
    SharedTable {
        name: "federation_report_nonces",
        columns: FEDERATION_REPORT_NONCES,
    },
    SharedTable {
        name: "industrial_message_seq",
        columns: INDUSTRIAL_MESSAGE_SEQ,
    },
    SharedTable {
        name: "operators",
        columns: OPERATORS,
    },
    SharedTable {
        name: "api_principals",
        columns: API_PRINCIPALS,
    },
    SharedTable {
        name: "cert_principals",
        columns: CERT_PRINCIPALS,
    },
    SharedTable {
        name: "fabric_assets",
        columns: FABRIC_ASSETS,
    },
    SharedTable {
        name: "ota_campaigns",
        columns: OTA_CAMPAIGNS,
    },
    SharedTable {
        name: "node_artifact_status",
        columns: NODE_ARTIFACT_STATUS,
    },
    SharedTable {
        name: "av_subsystem_meta",
        columns: AV_SUBSYSTEM_META,
    },
];

/// Which backend a set of `LiveColumn`s was introspected from — selects the
/// dialect-appropriate type matcher inside `diff_table`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
}

/// A column as introspected from a LIVE backend schema (dialect-normalized).
/// Both backends build a `Vec<LiveColumn>` per table and hand it to the SAME
/// `diff_table`, so the comparison logic is authored once.
#[derive(Debug, Clone)]
pub struct LiveColumn {
    pub name: String,
    /// The dialect-native declared type string (SQLite `PRAGMA table_info.type`
    /// or PG `information_schema.columns.data_type`).
    pub declared_type: String,
    /// The dialect's per-column NOT NULL flag (SQLite `notnull != 0`; PG
    /// `is_nullable = 'NO'`).
    pub not_null: bool,
    /// Whether the column participates in the table's PRIMARY KEY.
    pub primary_key: bool,
}

impl LiveColumn {
    /// A PK column is NOT NULL regardless of the per-column flag — this closes
    /// the SQLite `TEXT PRIMARY KEY` quirk (reported `notnull = 0`) so it
    /// compares equal to PG (where a PK column is always `is_nullable = NO`).
    fn effective_not_null(&self) -> bool {
        self.not_null || self.primary_key
    }
}

/// Diff ONE table's live columns against its spec, in both directions. Returns a
/// human-readable message per discrepancy (empty ⇒ conformant). `dialect`
/// selects which `LogicalType` matcher applies to the live declared types.
pub fn diff_table(table: &SharedTable, live: &[LiveColumn], dialect: Dialect) -> Vec<String> {
    let mut errors = Vec::new();

    if live.is_empty() {
        errors.push(format!(
            "shared table `{}` is MISSING from the {:?} backend",
            table.name, dialect
        ));
        return errors;
    }

    // spec → live: every declared column must exist with the right type,
    // nullability, and PK membership.
    for col in table.columns {
        let Some(lc) = live.iter().find(|c| c.name == col.name) else {
            errors.push(format!(
                "{}.{}: in spec but MISSING from the {:?} backend",
                table.name, col.name, dialect
            ));
            continue;
        };

        let type_ok = match dialect {
            Dialect::Sqlite => col.ty.matches_sqlite(&lc.declared_type),
            Dialect::Postgres => col.ty.matches_pg(&lc.declared_type),
        };
        if !type_ok {
            errors.push(format!(
                "{}.{}: {:?} type `{}` does not satisfy spec {}",
                table.name,
                col.name,
                dialect,
                lc.declared_type,
                col.ty.label()
            ));
        }

        let live_not_null = lc.effective_not_null();
        if live_not_null == col.nullable {
            errors.push(format!(
                "{}.{}: {:?} nullable={} but spec nullable={}",
                table.name, col.name, dialect, !live_not_null, col.nullable
            ));
        }

        if lc.primary_key != col.primary_key {
            errors.push(format!(
                "{}.{}: {:?} primary_key={} but spec primary_key={}",
                table.name, col.name, dialect, lc.primary_key, col.primary_key
            ));
        }
    }

    // live → spec: no un-spec'd columns (a column added to one backend without
    // updating the shared spec — and therefore the other backend — reds here).
    for lc in live {
        if !table.columns.iter().any(|c| c.name == lc.name) {
            errors.push(format!(
                "{}.{}: present in the {:?} backend but NOT in the shared spec \
                 (add it to schema_spec + the other backend, or it will drift)",
                table.name, lc.name, dialect
            ));
        }
    }

    errors
}

/// Introspect a live SQLite connection and assert every shared table conforms to
/// `SHARED_TABLES`. Returns the aggregated discrepancy report on mismatch.
pub fn assert_sqlite_conforms(conn: &Connection) -> Result<(), String> {
    let mut errors = Vec::new();
    for table in SHARED_TABLES {
        let live = match sqlite_live_columns(conn, table.name) {
            Ok(cols) => cols,
            Err(e) => {
                errors.push(format!("{}: PRAGMA table_info failed: {e}", table.name));
                continue;
            }
        };
        errors.extend(diff_table(table, &live, Dialect::Sqlite));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

/// Read a table's live columns from SQLite via `PRAGMA table_info`.
fn sqlite_live_columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<LiveColumn>> {
    // PRAGMA table_info columns: 0=cid, 1=name, 2=type, 3=notnull, 4=dflt, 5=pk.
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| {
        Ok(LiveColumn {
            name: r.get::<_, String>(1)?,
            declared_type: r.get::<_, String>(2)?,
            not_null: r.get::<_, i64>(3)? != 0,
            primary_key: r.get::<_, i64>(5)? > 0,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_type_matchers_cover_both_dialects() {
        assert!(Integer.matches_sqlite("INTEGER"));
        assert!(Integer.matches_sqlite("integer"));
        assert!(!Integer.matches_sqlite("TEXT"));
        assert!(Integer.matches_pg("bigint"));
        assert!(Integer.matches_pg("integer"));
        assert!(!Integer.matches_pg("boolean"));

        assert!(Text.matches_sqlite("TEXT"));
        assert!(Text.matches_pg("text"));

        assert!(Real.matches_sqlite("REAL"));
        assert!(Real.matches_pg("double precision"));

        // A bool is INTEGER in SQLite (indistinguishable from int there) but a
        // native boolean in PG.
        assert!(Bool.matches_sqlite("INTEGER"));
        assert!(Bool.matches_pg("boolean"));
        assert!(!Bool.matches_pg("bigint"));
    }

    #[test]
    fn spec_has_no_duplicate_tables_or_columns() {
        let mut seen_tables = std::collections::HashSet::new();
        for t in SHARED_TABLES {
            assert!(seen_tables.insert(t.name), "duplicate table {}", t.name);
            let mut seen_cols = std::collections::HashSet::new();
            let mut pk_count = 0;
            for c in t.columns {
                assert!(
                    seen_cols.insert(c.name),
                    "duplicate column {}.{}",
                    t.name,
                    c.name
                );
                if c.primary_key {
                    pk_count += 1;
                }
                // A PK column is never nullable in the spec.
                if c.primary_key {
                    assert!(
                        !c.nullable,
                        "{}.{} is PK but marked nullable",
                        t.name, c.name
                    );
                }
            }
            assert!(
                pk_count >= 1,
                "table {} has no primary key column in spec",
                t.name
            );
        }
    }

    /// The comparator flags a real divergence (a flipped nullability) and a
    /// spurious extra live column — proving `diff_table` is not vacuous.
    #[test]
    fn diff_table_detects_nullability_and_extra_column() {
        const COLS: &[SharedColumn] = &[pk("id", Text), nn("v", Integer)];
        let table = SharedTable {
            name: "t",
            columns: COLS,
        };
        // Conformant.
        let ok = vec![
            LiveColumn {
                name: "id".into(),
                declared_type: "TEXT".into(),
                not_null: false,
                primary_key: true,
            },
            LiveColumn {
                name: "v".into(),
                declared_type: "INTEGER".into(),
                not_null: true,
                primary_key: false,
            },
        ];
        assert!(diff_table(&table, &ok, Dialect::Sqlite).is_empty());

        // `v` wrongly nullable + an un-spec'd `extra` column.
        let bad = vec![
            LiveColumn {
                name: "id".into(),
                declared_type: "TEXT".into(),
                not_null: false,
                primary_key: true,
            },
            LiveColumn {
                name: "v".into(),
                declared_type: "INTEGER".into(),
                not_null: false,
                primary_key: false,
            },
            LiveColumn {
                name: "extra".into(),
                declared_type: "TEXT".into(),
                not_null: false,
                primary_key: false,
            },
        ];
        let errs = diff_table(&table, &bad, Dialect::Sqlite);
        assert_eq!(
            errs.len(),
            2,
            "expected nullability + extra-column errors, got {errs:?}"
        );
    }
}
