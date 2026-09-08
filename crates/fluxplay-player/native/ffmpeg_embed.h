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

/* Copy latest video frame scaled into tightly packed RGBA (out_w * out_h * 4).
 * Returns 1 if a frame was copied, 0 if none yet / error. */
int flux_ffmpeg_pull_rgba(FluxFfmpegPlayer *p, uint8_t *out, int out_w, int out_h);

/* Hint decode thread to scale toward this size (next frames). */
void flux_ffmpeg_set_output_size(FluxFfmpegPlayer *p, int w, int h);

int flux_ffmpeg_is_alive(FluxFfmpegPlayer *p);
int flux_ffmpeg_has_frame(FluxFfmpegPlayer *p);
void flux_ffmpeg_pause(FluxFfmpegPlayer *p, int paused);
void flux_ffmpeg_set_volume(FluxFfmpegPlayer *p, float volume01);
double flux_ffmpeg_position_secs(FluxFfmpegPlayer *p);
double flux_ffmpeg_duration_secs(FluxFfmpegPlayer *p);
/* Request seek; applied on decode thread. */
void flux_ffmpeg_seek(FluxFfmpegPlayer *p, double secs);

#ifdef __cplusplus
}
#endif
#endif
