use std::collections::HashMap;

use parko_core::backend::{InferenceBackend, TensorBatch, TensorStorage};
use parko_onnx::OrtBackend;

#[test]
fn mnist_end_to_end_inference() {
    let model_path = "tests/data/mnist-12.onnx";

    let backend = OrtBackend::new(model_path)
        .expect("failed to construct OrtBackend");

    let model = backend
        .load_model(model_path)
        .expect("failed to introspect MNIST model");

    let input_name = "Input3";
    let output_name = "Plus214_Output_0";

    let input_shape = model
        .input_shapes
        .get(input_name)
        .expect("MNIST input node 'Input3' not found");
    let output_shape = model
        .output_shapes
        .get(output_name)
        .expect("MNIST output node 'Plus214_Output_0' not found");

    assert_eq!(input_shape, &vec![1, 1, 28, 28], "MNIST input shape mismatch");
    assert_eq!(output_shape, &vec![1, 10], "MNIST output shape mismatch");

    let total_elems: usize = input_shape.iter().product();
    let flat_image = vec![0.0f32; total_elems];

    let mut named = HashMap::new();
    named.insert(
        input_name.to_string(),
        TensorStorage::Borrowed(&flat_image),
    );

    let batch = TensorBatch {
        named_tensors: named,
        metadata: HashMap::new(),
    };

    let output = backend
        .run(&model, &batch)
        .expect("OrtBackend run() failed");

    let storage = output
        .named_tensors
        .get(output_name)
        .expect("missing MNIST output tensor");

    let scores = storage.as_slice();
    assert_eq!(scores.len(), 10, "expected 10-class output");

    for (i, s) in scores.iter().enumerate() {
        assert!(s.is_finite(), "non-finite score at index {}: {}", i, s);
    }

    println!("MNIST inference successful. Output: {:?}", scores);
}
