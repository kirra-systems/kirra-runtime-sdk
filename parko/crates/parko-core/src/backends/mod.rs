pub mod mock;

#[cfg(feature = "backend-tensorrt")]
pub mod tensorrt_stub;
#[cfg(feature = "backend-tensorrt")]
pub use tensorrt_stub::TensorRTStubBackend;

#[cfg(feature = "backend-qnn")]
pub mod qnn_stub;
#[cfg(feature = "backend-qnn")]
pub use qnn_stub::QnnStubBackend;

#[cfg(feature = "backend-tidl")]
pub mod tidl_stub;
#[cfg(feature = "backend-tidl")]
pub use tidl_stub::TidlStubBackend;

#[cfg(feature = "backend-openvino")]
pub mod openvino_stub;
#[cfg(feature = "backend-openvino")]
pub use openvino_stub::OpenVinoStubBackend;

#[cfg(feature = "backend-amd")]
pub mod amd_stub;
#[cfg(feature = "backend-amd")]
pub use amd_stub::AmdStubBackend;
