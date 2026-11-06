/* coffee-core C ABI (spec §9.2). Frames and semantics are the same as in WebAssembly. */
#ifndef COFFEE_CORE_H
#define COFFEE_CORE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
  const uint8_t *ptr;
  size_t len;
} ct_bytes;

/* Host callback. Receives a request frame (valid only during the call) and returns a response
 * frame allocated with ct_alloc; the core frees it. Return {NULL, 0} if callbacks are not
 * supported. Must not call ct_call on the same instance. */
typedef ct_bytes (*ct_host_fn)(void *user_data, ct_bytes request);

/* ABI version, currently 1. */
uint32_t ct_abi_version(void);

/* An instance owns compiled templates. Use it from one thread at a time. host may be NULL. */
void *ct_instance_new(ct_host_fn host, void *user_data);
void ct_instance_free(void *instance);

/* Executes a request frame; free the response with ct_free(response.ptr, response.len). */
ct_bytes ct_call(void *instance, ct_bytes request);

uint8_t *ct_alloc(size_t len);
void ct_free(uint8_t *ptr, size_t len);

#ifdef __cplusplus
}
#endif

#endif /* COFFEE_CORE_H */
