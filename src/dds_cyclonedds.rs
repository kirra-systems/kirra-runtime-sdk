// src/dds_cyclonedds.rs
//
// D1 (review item 19) — the REAL CycloneDDS actuator writer.
//
// ┌───────────────────────────────────────────────────────────────────────────┐
// │ BUILD GATING. This module is compiled ONLY under `--features cyclonedds`.   │
// │ It links the native CycloneDDS C library (`libddsc`), which is NOT present  │
// │ in the default CI/build container, so it is NEVER built by a default        │
// │ `cargo build`/`cargo test` and CANNOT regress the default path. It is built │
// │ and run on an integrator host that ships CycloneDDS — e.g. a ROS 2 Jazzy    │
// │ machine (Jazzy's default RMW is rmw_cyclonedds_cpp, so `libddsc` is on the  │
// │ library path). The QoS MAPPING and READ-BACK VALIDATION logic this writer   │
// │ depends on lives in `dds_bridge.rs` as pure functions and IS exercised in   │
// │ CI (`qos_to_cyclone_params` / `cyclone_params_to_qos` / `validate_qos_      │
// │ readback`); this file is the thin `unsafe extern "C"` glue around them.     │
// └───────────────────────────────────────────────────────────────────────────┘
//
// WHAT IT ADDS over the byte-framer in `dds_bridge.rs`: a writer created against
// the live middleware whose NEGOTIATED QoS is read back (`dds_get_qos`) and run
// through `validate_qos_readback` — so a middleware that silently downgrades an
// actuator topic (TransientLocal, dropped deadline, BestEffort, …) is refused at
// open time (fail-closed), not discovered after a stale command actuates.
//
// ADR-0006 Clause 3: this FFI is the documented C/C++ INTEGRATION BOUNDARY, not
// the governor hot path. Kirra owns the QoS gate + read-back; the integrator owns
// the topic type support (the generated `dds_topic_descriptor_t` for their
// actuator message) and the sample it publishes.

#![cfg(feature = "cyclonedds")]

use std::ffi::{c_char, c_void, CString};

use crate::dds_bridge::{
    cyclone_params_to_qos, qos_to_cyclone_params, validate_qos_readback, CycloneQosParams,
    DdsQosProfile, DdsQosViolation, QosReadbackError,
};

/// Opaque CycloneDDS `dds_qos_t`.
#[repr(C)]
struct DdsQos {
    _private: [u8; 0],
}

/// `DDS_DOMAIN_DEFAULT` — use the domain from config/env (`CYCLONEDDS_URI`).
const DDS_DOMAIN_DEFAULT: u32 = 0xffff_ffff;
/// Reliable `max_blocking_time` — CycloneDDS's own default is 100 ms.
const RELIABLE_MAX_BLOCKING_NS: i64 = 100 * 1_000_000;

// Native CycloneDDS C API (dds/dds.h). Signatures are part of the CycloneDDS ABI.
// `dds_entity_t` = int32, `dds_return_t` = int32, `dds_duration_t` = int64,
// `dds_domainid_t` = uint32; the QoS kind enums are C `int` (i32) out-params.
#[link(name = "ddsc")]
extern "C" {
    fn dds_create_participant(domain: u32, qos: *const DdsQos, listener: *const c_void) -> i32;
    fn dds_create_topic(
        participant: i32,
        descriptor: *const c_void,
        name: *const c_char,
        qos: *const DdsQos,
        listener: *const c_void,
    ) -> i32;
    fn dds_create_writer(
        participant_or_publisher: i32,
        topic: i32,
        qos: *const DdsQos,
        listener: *const c_void,
    ) -> i32;
    fn dds_write(writer: i32, data: *const c_void) -> i32;
    fn dds_delete(entity: i32) -> i32;

    fn dds_create_qos() -> *mut DdsQos;
    fn dds_delete_qos(qos: *mut DdsQos);
    fn dds_qset_durability(qos: *mut DdsQos, kind: i32);
    fn dds_qset_history(qos: *mut DdsQos, kind: i32, depth: i32);
    fn dds_qset_reliability(qos: *mut DdsQos, kind: i32, max_blocking_time: i64);
    fn dds_qset_lifespan(qos: *mut DdsQos, lifespan: i64);
    fn dds_qset_deadline(qos: *mut DdsQos, deadline: i64);
    fn dds_qset_liveliness(qos: *mut DdsQos, kind: i32, lease_duration: i64);

    fn dds_get_qos(entity: i32, qos: *mut DdsQos) -> i32;
    fn dds_qget_durability(qos: *const DdsQos, kind: *mut i32) -> bool;
    fn dds_qget_history(qos: *const DdsQos, kind: *mut i32, depth: *mut i32) -> bool;
    fn dds_qget_reliability(qos: *const DdsQos, kind: *mut i32, max_blocking_time: *mut i64)
        -> bool;
    fn dds_qget_lifespan(qos: *const DdsQos, lifespan: *mut i64) -> bool;
    fn dds_qget_deadline(qos: *const DdsQos, deadline: *mut i64) -> bool;
    fn dds_qget_liveliness(qos: *const DdsQos, kind: *mut i32, lease_duration: *mut i64) -> bool;
}

/// Why the CycloneDDS actuator writer refused to come up (or to publish). Every
/// variant is fail-closed — no writer is handed back on error.
#[derive(Debug)]
pub enum CycloneDdsError {
    /// The REQUESTED profile is itself inadmissible (caught before touching DDS).
    RequestInadmissible(DdsQosViolation),
    /// A `dds_create_*` call returned a negative `dds_return_t`.
    EntityCreate { what: &'static str, code: i32 },
    /// `dds_create_qos` returned NULL.
    QosAlloc,
    /// `dds_get_qos` failed, or a `dds_qget_*` getter reported the policy absent —
    /// we cannot prove the negotiated QoS, so we refuse.
    ReadbackUnavailable,
    /// The negotiated QoS used a kind Kirra's model can't represent.
    ReadbackUnrepresentable,
    /// The negotiated QoS is weaker than requested on a safety axis.
    Readback(QosReadbackError),
    /// `dds_write` returned a negative `dds_return_t`.
    Publish(i32),
}

/// A CycloneDDS writer for one actuator topic, created with the Kirra actuator
/// QoS and proven (read-back) to have negotiated it. Owns its participant /
/// topic / writer entities and tears them down on drop.
pub struct CycloneDdsActuatorWriter {
    participant: i32,
    topic: i32,
    writer: i32,
    negotiated: DdsQosProfile,
}

impl CycloneDdsActuatorWriter {
    /// Create a writer for `topic_name` of the integrator-supplied type
    /// `descriptor` (a `*const dds_topic_descriptor_t` from their generated type
    /// support), under `profile`. Fails closed if the request is inadmissible, if
    /// any entity fails to create, or if the writer's READ-BACK QoS is weaker than
    /// requested.
    ///
    /// # Safety
    /// `descriptor` must be a valid, 'static `dds_topic_descriptor_t` pointer for
    /// the topic type (as produced by CycloneDDS IDL / rosidl type support).
    pub unsafe fn create(
        profile: &DdsQosProfile,
        topic_name: &str,
        descriptor: *const c_void,
    ) -> Result<Self, CycloneDdsError> {
        // Refuse a bad request before we ever talk to the middleware.
        profile
            .actuator_admissibility()
            .map_err(CycloneDdsError::RequestInadmissible)?;

        let qos = build_qos(profile)?;
        // From here on, `qos` and any created entities must be cleaned up on every
        // error path. `Guard` handles the entities; qos is deleted before return.
        let participant = dds_create_participant(DDS_DOMAIN_DEFAULT, std::ptr::null(), std::ptr::null());
        if participant < 0 {
            dds_delete_qos(qos);
            return Err(CycloneDdsError::EntityCreate { what: "participant", code: participant });
        }
        let mut guard = Guard { entity: participant };

        let cname = CString::new(topic_name).map_err(|_| {
            dds_delete_qos(qos);
            CycloneDdsError::EntityCreate { what: "topic_name_nul", code: 0 }
        })?;
        let topic = dds_create_topic(participant, descriptor, cname.as_ptr(), qos, std::ptr::null());
        if topic < 0 {
            dds_delete_qos(qos);
            return Err(CycloneDdsError::EntityCreate { what: "topic", code: topic });
        }

        let writer = dds_create_writer(participant, topic, qos, std::ptr::null());
        dds_delete_qos(qos); // the writer copied what it needs
        if writer < 0 {
            return Err(CycloneDdsError::EntityCreate { what: "writer", code: writer });
        }

        // READ-BACK: prove the writer negotiated at least the requested strictness.
        let negotiated = read_back_qos(writer)?;
        validate_qos_readback(profile, &negotiated).map_err(CycloneDdsError::Readback)?;

        // All checks passed — disarm the participant guard (it drops as a no-op at
        // scope end); the writer/topic are children of the participant and are
        // deleted with it by `CycloneDdsActuatorWriter`'s own Drop.
        guard.entity = 0;
        Ok(Self { participant, topic, writer, negotiated })
    }

    /// The QoS the middleware actually negotiated (proven read-back-admissible).
    pub fn negotiated_qos(&self) -> DdsQosProfile {
        self.negotiated
    }

    /// Publish one sample of the topic type. `sample` is a pointer to an instance
    /// of the integrator's generated actuator message (CycloneDDS serializes it
    /// using the topic's type support). The QoS gate was enforced at `create`.
    ///
    /// # Safety
    /// `sample` must point to a valid instance of the topic's C type.
    pub unsafe fn publish_sample(&self, sample: *const c_void) -> Result<(), CycloneDdsError> {
        let rc = dds_write(self.writer, sample);
        if rc < 0 {
            return Err(CycloneDdsError::Publish(rc));
        }
        Ok(())
    }
}

impl Drop for CycloneDdsActuatorWriter {
    fn drop(&mut self) {
        // Deleting the participant cascades to its topic + writer children.
        unsafe {
            let _ = dds_delete(self.writer);
            let _ = dds_delete(self.topic);
            let _ = dds_delete(self.participant);
        }
    }
}

/// Deletes a created entity if still armed (set `entity = 0` to disarm). Ensures
/// an early-return between participant creation and success doesn't leak it.
struct Guard {
    entity: i32,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.entity > 0 {
            unsafe {
                let _ = dds_delete(self.entity);
            }
        }
    }
}

/// Allocate + populate a `dds_qos_t` from the actuator profile via the pure
/// `qos_to_cyclone_params` mapping.
unsafe fn build_qos(profile: &DdsQosProfile) -> Result<*mut DdsQos, CycloneDdsError> {
    let p = qos_to_cyclone_params(profile);
    let qos = dds_create_qos();
    if qos.is_null() {
        return Err(CycloneDdsError::QosAlloc);
    }
    dds_qset_durability(qos, p.durability_kind);
    dds_qset_history(qos, p.history_kind, p.history_depth);
    dds_qset_reliability(qos, p.reliability_kind, RELIABLE_MAX_BLOCKING_NS);
    dds_qset_lifespan(qos, p.lifespan_ns);
    dds_qset_deadline(qos, p.deadline_ns);
    dds_qset_liveliness(qos, p.liveliness_kind, p.liveliness_lease_ns);
    Ok(qos)
}

/// Read a live writer's QoS back into a `DdsQosProfile` via `dds_get_qos` +
/// `dds_qget_*`, going through the pure `cyclone_params_to_qos` mapping. Any
/// failed getter or unrepresentable kind is fail-closed.
unsafe fn read_back_qos(writer: i32) -> Result<DdsQosProfile, CycloneDdsError> {
    let qos = dds_create_qos();
    if qos.is_null() {
        return Err(CycloneDdsError::QosAlloc);
    }
    // Always delete `qos` before returning.
    let result = (|| {
        if dds_get_qos(writer, qos) < 0 {
            return Err(CycloneDdsError::ReadbackUnavailable);
        }
        let mut durability_kind = -1;
        let mut history_kind = -1;
        let mut history_depth = -1;
        let mut reliability_kind = -1;
        let mut max_blocking = -1;
        let mut lifespan_ns = -1;
        let mut deadline_ns = -1;
        let mut liveliness_kind = -1;
        let mut liveliness_lease_ns = -1;

        let ok = dds_qget_durability(qos, &mut durability_kind)
            && dds_qget_history(qos, &mut history_kind, &mut history_depth)
            && dds_qget_reliability(qos, &mut reliability_kind, &mut max_blocking)
            && dds_qget_lifespan(qos, &mut lifespan_ns)
            && dds_qget_deadline(qos, &mut deadline_ns)
            && dds_qget_liveliness(qos, &mut liveliness_kind, &mut liveliness_lease_ns);
        if !ok {
            return Err(CycloneDdsError::ReadbackUnavailable);
        }

        let params = CycloneQosParams {
            durability_kind,
            history_kind,
            history_depth,
            reliability_kind,
            lifespan_ns,
            deadline_ns,
            liveliness_kind,
            liveliness_lease_ns,
        };
        cyclone_params_to_qos(&params).ok_or(CycloneDdsError::ReadbackUnrepresentable)
    })();
    dds_delete_qos(qos);
    result
}
