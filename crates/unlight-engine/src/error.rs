#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("no usable Vulkan adapter found")]
    NoAdapter,
    #[error("the adapter is missing a required feature: {0}")]
    MissingFeature(&'static str),
    #[error("could not open the GPU device: {0}")]
    DeviceRequest(String),
    #[error("the GPU device was lost: {0}")]
    DeviceLost(String),
    #[error("GPU error: {0}")]
    Gpu(String),
    #[error("{width}×{height} is too large for this GPU, the limit is {limit} per side")]
    ImageTooLarge { width: u32, height: u32, limit: u32 },
    #[error("too many device losses during export ({0}), giving up")]
    RecoveryExhausted(u32),
}

pub type Result<T> = std::result::Result<T, EngineError>;
