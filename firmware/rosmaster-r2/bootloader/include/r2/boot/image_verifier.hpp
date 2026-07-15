#pragma once

#include <cstddef>
#include <cstdint>

namespace r2::boot {

struct ImageManifest {
    std::uint32_t product_id;
    std::uint32_t hardware_compatibility;
    std::uint32_t image_length;
    std::uint32_t security_version;
    std::uint8_t digest_sha256[32];
    std::uint8_t signature_ed25519[64];
};

// NOTE: software integrity + version rollback checks only. On the STM32F103 the
// verifier key and bootloader live in mutable flash with no hardware root of
// trust, so this is NOT hardware-backed secure boot / tamper-resistant rollback
// (see PR scope note). A reviewed crypto backend + a provisioned RoT remain
// explicit phase gates.
enum class VerificationResult : std::uint8_t {
    // Fail-closed: `rejected` is value 0 so a default-constructed / zeroed /
    // memset verdict reads as a rejection, never as `accepted`. Every `verify`
    // implementation MUST return an explicit non-`rejected` value only on a
    // fully-passed check.
    rejected = 0,
    accepted,
    malformed_manifest,
    wrong_product,
    incompatible_hardware,
    invalid_length,
    digest_mismatch,
    invalid_signature,
    rollback_attempt,
};

// The zero/default verdict must be the reject state (fail-closed default).
static_assert(static_cast<std::uint8_t>(VerificationResult::rejected) == 0U,
              "a default/zero-initialized image verdict must fail closed (reject)");

class ImageReader {
public:
    virtual ~ImageReader() = default;
    [[nodiscard]] virtual bool read(std::uint32_t offset,
                                    std::uint8_t* destination,
                                    std::size_t length) const noexcept = 0;
};

class ImageVerifier {
public:
    virtual ~ImageVerifier() = default;
    [[nodiscard]] virtual VerificationResult verify(
        const ImageManifest& manifest,
        const ImageReader& image,
        std::uint32_t minimum_security_version) const noexcept = 0;
};

}  // namespace r2::boot
