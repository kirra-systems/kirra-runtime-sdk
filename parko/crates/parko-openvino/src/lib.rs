// crates/parko-openvino/src/lib.rs
//
// OpenVINO inference backend for parko-core. Implements
// `parko_core::backend::InferenceBackend` so the `InferenceLoop` +
// `GovernorComparator` + parko-ros2 transport work unchanged with
// this backend.
//
// Pattern mirrors `parko-onnx::OrtBackend`:
//   - Construction loads + compiles the model.
//   - `load_model(path)` introspects + returns a `ModelHandle` (the
//     shape info the caller binds inputs to).
//   - `run()` accepts a `TensorBatch`, runs inference, returns owned
//     outputs as a fresh `TensorBatch<'static>`.
//
// Runtime: built against `openvino = "0.11"` with the
// `runtime-linking` feature so the workspace COMPILES without the
// OpenVINO toolkit installed. The C++ runtime is dlopen'd at
// `OvBackend::new` — that call FAILS if libopenvino_c.so is not
// discoverable. Install path: see the crate README +
// `docs/safety/PARKO_OCCY_TOPOLOGY.md`. The crate uses the
// `openvino-finder` to locate the library; setting
// `OPENVINO_LIB_PATH` overrides discovery the same way
// `ORT_DYLIB_PATH` overrides the ort discovery in parko-onnx.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use openvino::{Core, CompiledModel, DeviceType, ElementType, Shape, Tensor};

use parko_core::backend::{
    BackendCapabilities, BackendDescriptor, BackendError, InferenceBackend,
    ModelHandle, PrecisionMode, TensorBatch, TensorStorage,
};

/// Native OpenVINO backend.
///
/// Carries the compiled model + Core in interior mutability so
/// `InferenceBackend::run` can take `&self`. OpenVINO's
/// `InferRequest::set_input_tensor` / `infer` / `get_output_tensor`
/// is the C++ stateful path; we wrap it in a `Mutex` because the
/// trait requires `Send + Sync`.
///
/// Inputs and outputs are introspected at construction time and
/// cached so `load_model` returns shapes in O(1).
pub struct OvBackend {
    state: Arc<Mutex<OvState>>,
    /// Cached at construction so the trait methods don't need to
    /// re-introspect on every call.
    input_specs:  Vec<NodeSpec>,
    output_specs: Vec<NodeSpec>,
    /// Path the backend was loaded from; threaded into ModelHandle's
    /// `model_id` for audit-log traceability.
    model_path: String,
}

struct OvState {
    /// We hold the Core for the lifetime of the backend. OpenVINO's
    /// FFI requires it to outlive the CompiledModel.
    #[allow(dead_code)]
    core: Core,
    compiled: CompiledModel,
}

#[derive(Debug, Clone)]
struct NodeSpec {
    name:  String,
    shape: Vec<usize>,
}

impl OvBackend {
    /// Construct the backend by loading + compiling an ONNX model on
    /// the CPU device. OpenVINO ingests ONNX directly — no separate
    /// IR conversion needed. (For OpenVINO's native IR `.xml + .bin`
    /// pairs, this constructor expects the `.xml` path; the
    /// weights-path argument is empty when the model is single-file
    /// like ONNX.)
    pub fn new(model_path: &str) -> Result<Self, BackendError> {
        let mut core = Core::new()
            .map_err(|e| BackendError::InitializationError(
                format!("openvino Core::new failed: {e:?} \
                         (is libopenvino_c.so installed and discoverable? \
                         Try setting OPENVINO_LIB_PATH.)")
            ))?;
        // ONNX files are self-contained — pass an empty weights path.
        // OpenVINO recognises the .onnx extension and uses its ONNX
        // frontend.
        let model = core
            .read_model_from_file(model_path, "")
            .map_err(|e| BackendError::InitializationError(
                format!("openvino read_model({model_path}) failed: {e:?}")
            ))?;

        let compiled = core
            .compile_model(&model, DeviceType::CPU)
            .map_err(|e| BackendError::InitializationError(
                format!("openvino compile_model(CPU) failed: {e:?}")
            ))?;

        // Pre-introspect input/output specs so `load_model` is fast.
        let input_specs  = introspect_inputs(&compiled)?;
        let output_specs = introspect_outputs(&compiled)?;

        // Sanity: a model with no inputs is meaningless.
        if input_specs.is_empty() {
            return Err(BackendError::InitializationError(
                format!("openvino: model {model_path} has zero inputs")
            ));
        }

        let state = Arc::new(Mutex::new(OvState { core, compiled }));

        Ok(Self {
            state,
            input_specs,
            output_specs,
            model_path: model_path.to_string(),
        })
    }
}

fn introspect_inputs(compiled: &CompiledModel) -> Result<Vec<NodeSpec>, BackendError> {
    let count = compiled.get_input_size().map_err(|e|
        BackendError::InitializationError(format!("openvino get_input_size failed: {e:?}")))?;
    let mut specs = Vec::with_capacity(count);
    for i in 0..count {
        let node = compiled.get_input_by_index(i).map_err(|e|
            BackendError::InitializationError(format!("openvino get_input_by_index({i}) failed: {e:?}")))?;
        let name = node.get_name().map_err(|e|
            BackendError::InitializationError(format!("openvino input[{i}].get_name failed: {e:?}")))?;
        let shape = shape_to_vec(&node.get_shape().map_err(|e|
            BackendError::InitializationError(format!("openvino input[{i}].get_shape failed: {e:?}")))?);
        specs.push(NodeSpec { name, shape });
    }
    Ok(specs)
}

fn introspect_outputs(compiled: &CompiledModel) -> Result<Vec<NodeSpec>, BackendError> {
    let count = compiled.get_output_size().map_err(|e|
        BackendError::InitializationError(format!("openvino get_output_size failed: {e:?}")))?;
    let mut specs = Vec::with_capacity(count);
    for i in 0..count {
        let node = compiled.get_output_by_index(i).map_err(|e|
            BackendError::InitializationError(format!("openvino get_output_by_index({i}) failed: {e:?}")))?;
        let name = node.get_name().map_err(|e|
            BackendError::InitializationError(format!("openvino output[{i}].get_name failed: {e:?}")))?;
        let shape = shape_to_vec(&node.get_shape().map_err(|e|
            BackendError::InitializationError(format!("openvino output[{i}].get_shape failed: {e:?}")))?);
        specs.push(NodeSpec { name, shape });
    }
    Ok(specs)
}

fn shape_to_vec(shape: &Shape) -> Vec<usize> {
    shape
        .get_dimensions()
        .iter()
        // Dimensions are signed (i64); a `-1` denotes a dynamic
        // dimension. Following the parko-onnx convention
        // (`shape.iter().map(|&d| d.max(1) as usize)`), we coerce
        // dynamic dims to 1 so the static-shape model handle is
        // usable for fixed-shape calls. Dynamic-shape models are
        // out of scope for parko's current safety contract.
        .map(|&d| if d <= 0 { 1usize } else { d as usize })
        .collect()
}

impl InferenceBackend for OvBackend {
    fn load_model(&self, path: &str) -> Result<ModelHandle, BackendError> {
        // OpenVINO's compile happens once in `new`; this method just
        // hands back the cached introspection. The `path` arg is
        // ignored — the same shape applies regardless of caller's
        // bookkeeping. Match parko-onnx's behaviour: include the
        // path in the `model_id` for audit traceability.
        let mut input_shapes  = HashMap::with_capacity(self.input_specs.len());
        for spec in &self.input_specs {
            input_shapes.insert(spec.name.clone(), spec.shape.clone());
        }
        let mut output_shapes = HashMap::with_capacity(self.output_specs.len());
        for spec in &self.output_specs {
            output_shapes.insert(spec.name.clone(), spec.shape.clone());
        }
        Ok(ModelHandle {
            model_id: format!("openvino_cpu_model_from_{}", path),
            input_shapes,
            output_shapes,
            expected_precision: PrecisionMode::FP32,
        })
    }

    fn run(
        &self,
        model: &ModelHandle,
        inputs: &TensorBatch,
    ) -> Result<TensorBatch<'static>, BackendError> {
        let mut state = self.state.lock()
            .map_err(|e| BackendError::ExecutionFailure(
                format!("openvino state lock poisoned: {e}")))?;

        // Re-create an InferRequest per call. OpenVINO recommends
        // recycling requests for throughput, but a per-call request
        // is simpler and matches parko-onnx's per-call pattern. The
        // CompiledModel itself is cached.
        let mut request = state.compiled.create_infer_request()
            .map_err(|e| BackendError::ExecutionFailure(
                format!("openvino create_infer_request failed: {e:?}")))?;

        // Bind inputs in declaration order. Names must match the
        // model's input nodes; missing names → DimensionMismatch
        // (the parko-core error type for "wrong shape supplied").
        for (idx, spec) in self.input_specs.iter().enumerate() {
            let storage = inputs.named_tensors.get(&spec.name).ok_or_else(||
                BackendError::ExecutionFailure(
                    format!("openvino: missing input tensor '{}'", spec.name)))?;
            let raw = storage.as_slice();
            let expected: usize = spec.shape.iter().product();
            if raw.len() != expected {
                return Err(BackendError::DimensionMismatch {
                    expected: spec.shape.clone(),
                    actual:   vec![raw.len()],
                });
            }
            // Build an FP32 tensor with the spec's shape, then copy
            // the input data into its backing buffer.
            let shape_obj = Shape::new(&spec.shape.iter().map(|&d| d as i64).collect::<Vec<_>>())
                .map_err(|e| BackendError::ExecutionFailure(
                    format!("openvino Shape::new({:?}) failed: {e:?}", spec.shape)))?;
            let mut tensor = Tensor::new(ElementType::F32, &shape_obj)
                .map_err(|e| BackendError::ExecutionFailure(
                    format!("openvino Tensor::new failed: {e:?}")))?;
            {
                let buf: &mut [f32] = tensor.get_data_mut::<f32>()
                    .map_err(|e| BackendError::ExecutionFailure(
                        format!("openvino tensor get_data_mut failed: {e:?}")))?;
                if buf.len() != raw.len() {
                    return Err(BackendError::ShapeMismatch {
                        expected: buf.len(),
                        got:      raw.len(),
                    });
                }
                buf.copy_from_slice(raw);
            }
            request.set_input_tensor_by_index(idx, &tensor)
                .map_err(|e| BackendError::ExecutionFailure(
                    format!("openvino set_input_tensor_by_index({idx}) failed: {e:?}")))?;
        }

        request.infer()
            .map_err(|e| BackendError::ExecutionFailure(
                format!("openvino infer failed: {e:?}")))?;

        // Collect outputs by index → owned Vec<f32>.
        let mut output_named = HashMap::with_capacity(self.output_specs.len());
        for (idx, spec) in self.output_specs.iter().enumerate() {
            let tensor = request.get_output_tensor_by_index(idx)
                .map_err(|e| BackendError::ExecutionFailure(
                    format!("openvino get_output_tensor_by_index({idx}) failed: {e:?}")))?;
            let data = tensor.get_data::<f32>()
                .map_err(|e| BackendError::ExecutionFailure(
                    format!("openvino output[{idx}].get_data::<f32> failed: {e:?}")))?;
            output_named.insert(spec.name.clone(), TensorStorage::Owned(data.to_vec()));
        }

        // We accepted `model: &ModelHandle` for parity with parko-onnx
        // but our state already carries the compiled model. The arg
        // is used only for the assertion below to surface a clear
        // error if the caller swaps in a stale handle from another
        // backend.
        debug_assert!(
            model.input_shapes.len() == self.input_specs.len(),
            "ModelHandle input_shapes ({}) disagrees with the OvBackend's compiled inputs ({})",
            model.input_shapes.len(), self.input_specs.len()
        );

        Ok(TensorBatch {
            named_tensors: output_named,
            metadata: HashMap::new(),
        })
    }

    fn capabilities(&self) -> BackendCapabilities {
        // CPU baseline matches parko-onnx — neither runtime exposes
        // int8/fp16 by default for stock ONNX models. Future work
        // (PARK-029) adds quantization-aware variants.
        BackendCapabilities {
            supports_int8:  false,
            supports_fp16:  false,
            max_batch_size: None,
        }
    }

    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor::IntelOpenVino
    }
}

impl OvBackend {
    /// Audit-log helper — returns the model path the backend was
    /// loaded from. Used by integration tests; not part of the
    /// public InferenceBackend trait.
    pub fn model_path(&self) -> &str {
        &self.model_path
    }
}
