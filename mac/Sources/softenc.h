// Software video encoders (AV1 via SVT-AV1, VP9 via libvpx) behind a four-function C API, so Swift never touches
// the vendor structs. When the libraries aren't available at build time everything compiles to a stub:
// sb_soft_available() returns 0 and the app offers H.264 and HEVC only.
#ifndef SB_SOFTENC_H
#define SB_SOFTENC_H

#include <stddef.h>
#include <stdint.h>

typedef struct SBSoftEncoder SBSoftEncoder;

enum { SB_CODEC_AV1 = 2, SB_CODEC_VP9 = 3 }; // same ids as the wire format's codec field

/// 1 if this build can encode `codec` in software.
int sb_soft_available(int codec);

/// Creates a low-latency CBR encoder. NULL on failure, with a message in `err`.
SBSoftEncoder *sb_soft_create(int codec, int width, int height, int fps, int kbps, char *err, size_t err_len);

/// Encodes one NV12 picture (`y` plane, interleaved `uv` plane). Returns the number of bytes written to `out`
/// (0 = encoder produced nothing for this picture), or a negative value on error. `*is_key` reports a keyframe.
int sb_soft_encode(SBSoftEncoder *e, const uint8_t *y, int y_stride, const uint8_t *uv, int uv_stride,
                   int force_key, uint8_t *out, size_t out_cap, int *is_key);

/// Changes the target bitrate. Returns 0 on success.
int sb_soft_set_bitrate(SBSoftEncoder *e, int kbps);

void sb_soft_destroy(SBSoftEncoder *e);

#endif
