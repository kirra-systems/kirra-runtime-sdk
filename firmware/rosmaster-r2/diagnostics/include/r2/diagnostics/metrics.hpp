#pragma once

#include <array>
#include <cstddef>
#include <cstdint>

namespace r2::diagnostics {

// Single-writer accumulator. It is intentionally not atomic: the owning
// control/ISR context must publish an immutable copy through a bounded critical
// section or versioned SPSC handoff before another task reads it.
class TimingHistogram {
public:
    static constexpr std::size_t kBucketCount = 16U;

    void observe(const std::uint32_t duration_us,
                 const std::uint32_t deadline_us) noexcept {
        ++samples_;
        if (duration_us > maximum_us_) {
            maximum_us_ = duration_us;
        }
        if (duration_us > deadline_us) {
            ++deadline_misses_;
        }

        std::size_t bucket = 0U;
        auto threshold = 1U;
        while (bucket + 1U < kBucketCount && duration_us > threshold) {
            threshold <<= 1U;
            ++bucket;
        }
        ++buckets_[bucket];
    }

    [[nodiscard]] std::uint64_t samples() const noexcept { return samples_; }
    [[nodiscard]] std::uint64_t deadline_misses() const noexcept {
        return deadline_misses_;
    }
    [[nodiscard]] std::uint32_t maximum_us() const noexcept { return maximum_us_; }
    [[nodiscard]] const std::array<std::uint32_t, kBucketCount>& buckets() const noexcept {
        return buckets_;
    }

private:
    std::array<std::uint32_t, kBucketCount> buckets_{};
    std::uint64_t samples_{0U};
    std::uint64_t deadline_misses_{0U};
    std::uint32_t maximum_us_{0U};
};

struct RuntimeHealth {
    std::uint16_t cpu_load_permille{0U};
    std::uint16_t minimum_idle_stack_bytes{0U};
    std::uint16_t minimum_control_stack_bytes{0U};
    std::uint32_t communication_crc_errors{0U};
    std::uint32_t communication_sequence_gaps{0U};
    std::uint32_t encoder_overflows{0U};
    std::uint32_t watchdog_near_misses{0U};
    TimingHistogram control_loop;
    TimingHistogram command_latency;
};

}  // namespace r2::diagnostics
