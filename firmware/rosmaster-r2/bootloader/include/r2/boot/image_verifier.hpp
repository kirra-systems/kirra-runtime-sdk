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

enum class VerificationResult : std::uint8_t {
    accepted,
    malformed_manifest,
    wrong_product,
    incompatible_hardware,
    invalid_length,
    digest_mismatch,
    invalid_signature,
    rollback_attempt,
};

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
