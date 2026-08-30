#ifndef ACG_WASMD_SCHEDULER_FFI_H
#define ACG_WASMD_SCHEDULER_FFI_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct AcgWasmdScheduler AcgWasmdScheduler;

typedef struct AcgByteBuffer {
  uint8_t *ptr;
  size_t len;
  size_t cap;
} AcgByteBuffer;

AcgWasmdScheduler *acg_wasmd_scheduler_new(const uint8_t *json_ptr,
                                            size_t json_len,
                                            AcgByteBuffer *error_out);
int32_t acg_wasmd_scheduler_plan(AcgWasmdScheduler *scheduler,
                                 const uint8_t *json_ptr,
                                 size_t json_len,
                                 AcgByteBuffer *result_out,
                                 AcgByteBuffer *error_out);
int32_t acg_wasmd_scheduler_feedback(AcgWasmdScheduler *scheduler,
                                     const uint8_t *json_ptr,
                                     size_t json_len,
                                     AcgByteBuffer *result_out,
                                     AcgByteBuffer *error_out);
void acg_wasmd_scheduler_free(AcgWasmdScheduler *scheduler);
void acg_wasmd_scheduler_buffer_free(AcgByteBuffer buffer);

#ifdef __cplusplus
}
#endif

#endif
