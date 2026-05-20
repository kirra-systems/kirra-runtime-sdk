// src/startup_sentinel.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupTrustState {
    Trusted,
    HardwareRootUnavailable,
    TpmUnresponsive,
}

pub struct StartupSentinel;

impl StartupSentinel {
    /// Returns Trusted if the TPM is healthy, or if the tpm feature is not compiled in.
    /// All other states are fail-closed — the caller must not proceed.
    pub fn verify_hardware_root() -> StartupTrustState {
        #[cfg(feature = "tpm")]
        {
            match crate::tpm::TpmBootstrap::new() {
                Ok(mut tpm) => {
                    if tpm.verify_readiness().is_ok() {
                        StartupTrustState::Trusted
                    } else {
                        StartupTrustState::TpmUnresponsive
                    }
                }
                Err(_) => StartupTrustState::HardwareRootUnavailable,
            }
        }

        #[cfg(not(feature = "tpm"))]
        {
            StartupTrustState::Trusted
        }
    }
}
