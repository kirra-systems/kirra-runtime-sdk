#pragma once

// REFERENCE IMPLEMENTATION — NOT production-vetted crypto.
//
// SHA-256 + HMAC-SHA-256 truncated to 128 bits, allocation-free, C++17.
// This file provides the link-layer MAC seam described in docs/PROTOCOL.md
// ("Flags", AUTH_TAG bit) and is intentionally isolated for future replacement
// with a reviewed, target-benchmarked library (see PROTOCOL.md phase-gate note).
//
// Side-channel properties:
//   • The tag *comparison* (constant_time_equal) uses a volatile accumulator to
//     prevent the compiler from inserting an early-exit branch over elements.
//   • SHA-256 itself is NOT hardened against timing or cache-based side channels.
//   • Replace with a vetted constant-time hash primitive before hardware deployment.
//
// Include this header only from protocol/src/wire.cpp.

#include <array>
#include <cstddef>
#include <cstdint>

namespace r2::protocol::internal {

// ---------------------------------------------------------------------------
// SHA-256 constants (NIST FIPS 180-4, Section 4.2.2)
// ---------------------------------------------------------------------------

inline constexpr std::array<std::uint32_t, 64U> kSha256K{{
    0x428a2f98U, 0x71374491U, 0xb5c0fbcfU, 0xe9b5dba5U,
    0x3956c25bU, 0x59f111f1U, 0x923f82a4U, 0xab1c5ed5U,
    0xd807aa98U, 0x12835b01U, 0x243185beU, 0x550c7dc3U,
    0x72be5d74U, 0x80deb1feU, 0x9bdc06a7U, 0xc19bf174U,
    0xe49b69c1U, 0xefbe4786U, 0x0fc19dc6U, 0x240ca1ccU,
    0x2de92c6fU, 0x4a7484aaU, 0x5cb0a9dcU, 0x76f988daU,
    0x983e5152U, 0xa831c66dU, 0xb00327c8U, 0xbf597fc7U,
    0xc6e00bf3U, 0xd5a79147U, 0x06ca6351U, 0x14292967U,
    0x27b70a85U, 0x2e1b2138U, 0x4d2c6dfcU, 0x53380d13U,
    0x650a7354U, 0x766a0abbU, 0x81c2c92eU, 0x92722c85U,
    0xa2bfe8a1U, 0xa81a664bU, 0xc24b8b70U, 0xc76c51a3U,
    0xd192e819U, 0xd6990624U, 0xf40e3585U, 0x106aa070U,
    0x19a4c116U, 0x1e376c08U, 0x2748774cU, 0x34b0bcb5U,
    0x391c0cb3U, 0x4ed8aa4aU, 0x5b9cca4fU, 0x682e6ff3U,
    0x748f82eeU, 0x78a5636fU, 0x84c87814U, 0x8cc70208U,
    0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U,
}};

inline constexpr std::array<std::uint32_t, 8U> kSha256InitH{{
    0x6a09e667U, 0xbb67ae85U, 0x3c6ef372U, 0xa54ff53aU,
    0x510e527fU, 0x9b05688cU, 0x1f83d9abU, 0x5be0cd19U,
}};

// ---------------------------------------------------------------------------
// SHA-256 streaming state
// ---------------------------------------------------------------------------

struct Sha256Ctx {
    std::array<std::uint32_t, 8U> h{};
    std::array<std::uint8_t, 64U> block{};
    std::size_t block_used{0U};
    std::uint64_t total_bytes{0U};
};

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

[[nodiscard]] inline std::uint32_t rotr32(const std::uint32_t x,
                                           const std::uint32_t n) noexcept {
    return (x >> n) | (x << (32U - n));
}

inline void sha256_compress(std::array<std::uint32_t, 8U>& h,
                             const std::array<std::uint8_t, 64U>& blk) noexcept {
    std::array<std::uint32_t, 64U> w{};

    // Load first 16 words, big-endian
    for (std::size_t i = 0U; i < 16U; ++i) {
        const std::size_t b = i * 4U;
        w[i] = (static_cast<std::uint32_t>(blk[b]) << 24U) |
               (static_cast<std::uint32_t>(blk[b + 1U]) << 16U) |
               (static_cast<std::uint32_t>(blk[b + 2U]) << 8U) |
               static_cast<std::uint32_t>(blk[b + 3U]);
    }

    // Extend message schedule
    for (std::size_t i = 16U; i < 64U; ++i) {
        const std::uint32_t wi15 = w[i - 15U];
        const std::uint32_t wi2  = w[i - 2U];
        const std::uint32_t s0 =
            rotr32(wi15, 7U) ^ rotr32(wi15, 18U) ^ (wi15 >> 3U);
        const std::uint32_t s1 =
            rotr32(wi2, 17U) ^ rotr32(wi2, 19U) ^ (wi2 >> 10U);
        w[i] = w[i - 16U] + s0 + w[i - 7U] + s1;
    }

    std::uint32_t a = h[0U]; std::uint32_t b = h[1U];
    std::uint32_t c = h[2U]; std::uint32_t d = h[3U];
    std::uint32_t e = h[4U]; std::uint32_t f = h[5U];
    std::uint32_t g = h[6U]; std::uint32_t hv = h[7U];

    for (std::size_t i = 0U; i < 64U; ++i) {
        const std::uint32_t s1  = rotr32(e, 6U) ^ rotr32(e, 11U) ^ rotr32(e, 25U);
        const std::uint32_t ch  = (e & f) ^ (~e & g);
        const std::uint32_t t1  = hv + s1 + ch + kSha256K[i] + w[i];
        const std::uint32_t s0  = rotr32(a, 2U) ^ rotr32(a, 13U) ^ rotr32(a, 22U);
        const std::uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
        const std::uint32_t t2  = s0 + maj;

        hv = g; g = f; f = e; e = d + t1;
        d  = c; c = b; b = a; a = t1 + t2;
    }

    h[0U] += a; h[1U] += b; h[2U] += c; h[3U] += d;
    h[4U] += e; h[5U] += f; h[6U] += g; h[7U] += hv;
}

// ---------------------------------------------------------------------------
// SHA-256 streaming API
// ---------------------------------------------------------------------------

inline void sha256_init(Sha256Ctx& ctx) noexcept {
    ctx.h          = kSha256InitH;
    ctx.block      = {};
    ctx.block_used = 0U;
    ctx.total_bytes = 0U;
}

inline void sha256_update(Sha256Ctx& ctx,
                           const std::uint8_t* data,
                           const std::size_t len) noexcept {
    for (std::size_t i = 0U; i < len; ++i) {
        ctx.block[ctx.block_used] = data[i];
        ++ctx.block_used;
        ++ctx.total_bytes;
        if (ctx.block_used == 64U) {
            sha256_compress(ctx.h, ctx.block);
            ctx.block_used = 0U;
        }
    }
}

[[nodiscard]] inline std::array<std::uint8_t, 32U>
sha256_finalize(Sha256Ctx& ctx) noexcept {
    // Append 0x80 padding byte (block_used is in [0, 63] at this point)
    ctx.block[ctx.block_used] = 0x80U;
    ++ctx.block_used;

    // If not enough room for the 8-byte big-endian bit length, flush and reset
    if (ctx.block_used > 56U) {
        for (std::size_t i = ctx.block_used; i < 64U; ++i) {
            ctx.block[i] = 0U;
        }
        sha256_compress(ctx.h, ctx.block);
        ctx.block_used = 0U;
    }

    // Zero-fill up to position 56, then write big-endian bit length
    for (std::size_t i = ctx.block_used; i < 56U; ++i) {
        ctx.block[i] = 0U;
    }
    const std::uint64_t bit_len = ctx.total_bytes * 8U;
    for (std::size_t i = 0U; i < 8U; ++i) {
        ctx.block[56U + i] =
            static_cast<std::uint8_t>(bit_len >> ((7U - i) * 8U));
    }
    sha256_compress(ctx.h, ctx.block);

    // Produce big-endian digest
    std::array<std::uint8_t, 32U> digest{};
    for (std::size_t i = 0U; i < 8U; ++i) {
        for (std::size_t j = 0U; j < 4U; ++j) {
            digest[i * 4U + j] =
                static_cast<std::uint8_t>(ctx.h[i] >> ((3U - j) * 8U));
        }
    }
    return digest;
}

// ---------------------------------------------------------------------------
// HMAC-SHA-256 truncated to 128 bits
//
// Computes HMAC-SHA-256 over `message` under `key` and returns the first
// 16 bytes of the full 32-byte tag.  Key length is fixed at 32 bytes
// (≤ SHA-256 block size of 64 bytes, so no key hashing is needed).
// ---------------------------------------------------------------------------

[[nodiscard]] inline std::array<std::uint8_t, 16U>
hmac_sha256_truncated(const std::array<std::uint8_t, 32U>& key,
                      const std::uint8_t* message,
                      const std::size_t message_len) noexcept {
    constexpr std::size_t kBlockSize = 64U;

    // Build key block: key (32 bytes) zero-padded to 64 bytes
    std::array<std::uint8_t, kBlockSize> key_block{};
    for (std::size_t i = 0U; i < key.size(); ++i) {
        key_block[i] = key[i];
    }
    // Remaining bytes of key_block are already 0

    // Inner hash: SHA-256( (key XOR ipad) || message )
    std::array<std::uint8_t, kBlockSize> ipad_key{};
    for (std::size_t i = 0U; i < kBlockSize; ++i) {
        ipad_key[i] = static_cast<std::uint8_t>(key_block[i] ^ 0x36U);
    }
    Sha256Ctx inner{};
    sha256_init(inner);
    sha256_update(inner, ipad_key.data(), kBlockSize);
    sha256_update(inner, message, message_len);
    const auto inner_hash = sha256_finalize(inner);

    // Outer hash: SHA-256( (key XOR opad) || inner_hash )
    std::array<std::uint8_t, kBlockSize> opad_key{};
    for (std::size_t i = 0U; i < kBlockSize; ++i) {
        opad_key[i] = static_cast<std::uint8_t>(key_block[i] ^ 0x5CU);
    }
    Sha256Ctx outer{};
    sha256_init(outer);
    sha256_update(outer, opad_key.data(), kBlockSize);
    sha256_update(outer, inner_hash.data(), inner_hash.size());
    const auto full_tag = sha256_finalize(outer);

    // Truncate to 128 bits (first 16 bytes)
    std::array<std::uint8_t, 16U> tag{};
    for (std::size_t i = 0U; i < 16U; ++i) {
        tag[i] = full_tag[i];
    }
    return tag;
}

// ---------------------------------------------------------------------------
// Constant-time 16-byte equality check
//
// The volatile accumulator forces all 16 XOR comparisons to execute even
// when a mismatch is detected early; the compiler cannot introduce an early
// exit.  This is the ONLY constant-time guarantee in this reference file.
// ---------------------------------------------------------------------------

[[nodiscard]] inline bool constant_time_equal(
    const std::array<std::uint8_t, 16U>& a,
    const std::array<std::uint8_t, 16U>& b) noexcept {
    volatile unsigned int acc = 0U;
    for (std::size_t i = 0U; i < 16U; ++i) {
        acc = static_cast<unsigned int>(acc) |
              (static_cast<unsigned int>(a[i]) ^
               static_cast<unsigned int>(b[i]));
    }
    return static_cast<unsigned int>(acc) == 0U;
}

}  // namespace r2::protocol::internal
