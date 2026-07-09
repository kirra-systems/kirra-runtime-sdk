// src/tpm.rs

#[cfg(feature = "tpm")]
use std::str::FromStr;
#[cfg(feature = "tpm")]
use tss_esapi::{Context, TctiNameConf};

#[cfg(feature = "tpm")]
pub struct TpmBootstrap {
    pub context: Context,
}

#[cfg(feature = "tpm")]
impl TpmBootstrap {
    // Reads TSS2_TCTI from env first; falls back to /dev/tpmrm0 (production default).
    pub fn new() -> Result<Self, &'static str> {
        let tcti = std::env::var("TSS2_TCTI")
            .ok()
            .filter(|s| !s.is_empty())
            .and_then(|s| TctiNameConf::from_str(&s).ok())
            .unwrap_or_else(|| TctiNameConf::Device(Default::default()));
        let context = Context::new(tcti).map_err(|_| "TPM_CONTEXT_INIT_FAILED")?;
        Ok(Self { context })
    }

    pub fn verify_readiness(&mut self) -> Result<(), &'static str> {
        self.context
            .get_random(1)
            .map_err(|_| "TPM_NOT_RESPONDING")?;
        Ok(())
    }
}
