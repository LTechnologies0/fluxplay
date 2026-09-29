/* In-process FFmpeg demux/decode → RGBA frames for iced (libmpv-style embed). */
#ifndef FLUX_FFMPEG_EMBED_H
#define FLUX_FFMPEG_EMBED_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct FluxFfmpegPlayer FluxFfmpegPlayer;

typedef struct FluxFfmpegOpenOpts {
    const char *url;
    const char *user_agent;
    const char *referer;
    const char *http_proxy;
    int low_latency;
    int hwdec;
} FluxFfmpegOpenOpts;

/* Opens URL and starts a decode thread. Returns NULL on failure. */
FluxFfmpegPlayer *flux_ffmpeg_open(const FluxFfmpegOpenOpts *opts);

/* Set libavutil log level (AV_LOG_*). Call before open for verbose demux/decode. */
void flux_ffmpeg_set_av_log_level(int level);

void flux_ffmpeg_close(FluxFfmpegPlayer *p);

/* Copy the latest frame. `out` must hold out_w * out_h * 4 bytes.
 * The copied frame may be smaller (no upscale); got_w/got_h receive its size.
 * Returns 1 if a frame was copied, 0 if none yet / error. */
int flux_ffmpeg_pull_rgba(FluxFfmpegPlayer *p, uint8_t *out, int out_w, int out_h, int *got_w, int *got_h);

/* Ready-frame size without consuming (0 if none). Used to size the pull buffer. */
int flux_ffmpeg_frame_size(FluxFfmpegPlayer *p, int *w, int *h);

/* Hint decode thread to scale toward this size (next frames). */
void flux_ffmpeg_set_output_size(FluxFfmpegPlayer *p, int w, int h);

/* Monitor refresh. Decode thread drops frames above this; the UI does not block. */
void flux_ffmpeg_set_present_hz(FluxFfmpegPlayer *p, int hz);

int flux_ffmpeg_is_alive(FluxFfmpegPlayer *p);
int flux_ffmpeg_has_frame(FluxFfmpegPlayer *p);
void flux_ffmpeg_pause(FluxFfmpegPlayer *p, int paused);
void flux_ffmpeg_set_volume(FluxFfmpegPlayer *p, float volume01);
double flux_ffmpeg_position_secs(FluxFfmpegPlayer *p);
double flux_ffmpeg_duration_secs(FluxFfmpegPlayer *p);
/* Request seek; applied on decode thread. */
void flux_ffmpeg_seek(FluxFfmpegPlayer *p, double secs);

/* Read-ahead state (any out pointer may be NULL). Returns 1 while playback
 * holds to rebuffer after the packet queue ran dry. */
int flux_ffmpeg_buffer_state(FluxFfmpegPlayer *p, double *buffered_secs, double *goal_secs,
                             double *net_mbps, double *media_mbps);

/* ── GPU video stage ────────────────────────────────────────────────────────
 * With YUV output on, frames keep their decoded size and planes (no RGBA
 * conversion, no CPU scaling): the UI converts and scales in a shader. With
 * GPU slots attached, CUDA frames never leave the GPU: at present time they
 * are copied device-to-device into a Vulkan buffer the UI samples from. */

enum { FLUX_FMT_RGBA = 0, FLUX_FMT_NV12 = 1, FLUX_FMT_P010 = 2 };
enum { FLUX_FRAME_CPU = 1, FLUX_FRAME_GPU = 2 };
enum { FLUX_MATRIX_BT601 = 0, FLUX_MATRIX_BT709 = 1, FLUX_MATRIX_BT2020 = 2 };

/* Planes: Y (`h` rows of `pitch` bytes) then interleaved UV (`(h+1)/2` rows of
 * `pitch` bytes). RGBA: one plane, already letterboxed to the requested size. */
typedef struct FluxFrameInfo {
    int kind;       /* FLUX_FRAME_* */
    int format;     /* FLUX_FMT_* */
    int width;
    int height;
    int pitch;
    int sar_num;
    int sar_den;
    int matrix;     /* FLUX_MATRIX_* */
    int full_range;
    int slot;       /* GPU slot holding the planes (kind == GPU) */
    uint64_t bytes; /* CPU bytes to pull (kind == CPU) */
} FluxFrameInfo;

/* 1: publish YUV planes at decoded size, 0: RGBA scaled to set_output_size. */
void flux_ffmpeg_set_yuv_output(FluxFfmpegPlayer *p, int on);

/* Describe the ready frame without consuming it. Returns 1 when one is ready. */
int flux_ffmpeg_frame_info(FluxFfmpegPlayer *p, FluxFrameInfo *info);

/* Consume the ready frame. CPU frames are copied to `out` (`cap` bytes, see
 * info.bytes). Returns 1 on success, 0 when none is ready or `cap` is short. */
int flux_ffmpeg_pull_frame(FluxFfmpegPlayer *p, FluxFrameInfo *info, uint8_t *out, uint64_t cap);

/* 1 when CUDA frames could stay on the GPU but no (large enough) slots are
 * attached; w/h/bpc (1 = NV12, 2 = P010) describe the frames. */
int flux_ffmpeg_gpu_need(FluxFfmpegPlayer *p, int *w, int *h, int *bpc);

/* Hand over `n` exported Vulkan memory fds (OPAQUE_FD, `slot_bytes` each).
 * `uuid` is the Vulkan physical device UUID: slots on another GPU are refused.
 * Takes ownership of every fd. Returns 1 when frames now go zero-copy. */
int flux_ffmpeg_gpu_attach(FluxFfmpegPlayer *p, const int *fds, int n, uint64_t slot_bytes,
                           int pitch, int w, int h, int bpc, const uint8_t *uuid);

/* A pulled GPU slot stays reserved until the UI has copied it out and calls
 * this; only then may decode overwrite it. */
void flux_ffmpeg_gpu_release(FluxFfmpegPlayer *p, int slot);

/* RGBA copy of the last GPU frame at decoded size (screenshots). `out` holds
 * `cap` bytes; returns 1 and the size on success. */
int flux_ffmpeg_gpu_snapshot(FluxFfmpegPlayer *p, uint8_t *out, uint64_t cap, int *w, int *h);

#ifdef __cplusplus
}
#endif
#endif
