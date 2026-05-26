#ifndef KIRRA_H
#define KIRRA_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

double kirra_filter_move_velocity(double demand, double dt);
double kirra_filter_rotate_velocity(double angular_demand, double dt);
uint32_t kirra_get_trust_score(void);
int kirra_reset_state(const uint8_t *token_ptr, size_t token_len);

#ifdef __cplusplus
}
#endif

#endif /* KIRRA_H */
