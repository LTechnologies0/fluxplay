/* Embedded FFmpeg player — demux + decode video to RGBA for iced software stage.
 * Audio is decoded, resampled to S16LE stereo, and played via pw-play/pacat.
 */
#include "ffmpeg_embed.h"

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/avutil.h>
#include <libavutil/channel_layout.h>
#include <libavutil/hwcontext.h>
#include <libavutil/imgutils.h>
#include <libavutil/opt.h>
#include <libavutil/time.h>
#include <libswresample/swresample.h>
#include <libswscale/swscale.h>

#include <libavutil/pixdesc.h>

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <time.h>
#include <stdlib.h>
#include <ctype.h>
#include <string.h>
#include <unistd.h>

/* CUDA driver API subset, resolved from libcuda.so.1 at runtime (no SDK at
 * build time, no hard dependency on NVIDIA). Layouts follow cuda.h. */
typedef int CUresult;
typedef int CUdevice;
typedef unsigned long long CUdeviceptr;
typedef struct CUctx_st *CUcontext;
typedef struct CUstream_st *CUstream;
typedef struct CUextMemory_st *CUexternalMemory;
typedef struct { char bytes[16]; } CUuuid;
#define CUDA_VERSION 12000
#include <libavutil/hwcontext_cuda.h>

enum { FLUX_CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD = 1 };
enum { FLUX_CU_MEMORYTYPE_DEVICE = 2 };

typedef struct {
    int type;
    union {
        int fd;
        struct {
            void *handle;
            const void *name;
        } win32;
        const void *nvSciBufObject;
    } handle;
    unsigned long long size;
    unsigned int flags;
    unsigned int reserved[16];
} FluxCuExtMemHandleDesc;

typedef struct {
    unsigned long long offset;
    unsigned long long size;
    unsigned int flags;
    unsigned int reserved[16];
} FluxCuExtMemBufferDesc;

typedef struct {
    size_t srcXInBytes, srcY;
    int srcMemoryType;
    const void *srcHost;
    CUdeviceptr srcDevice;
    void *srcArray;
    size_t srcPitch;
    size_t dstXInBytes, dstY;
    int dstMemoryType;
    void *dstHost;
    CUdeviceptr dstDevice;
    void *dstArray;
    size_t dstPitch;
    size_t WidthInBytes;
    size_t Height;
} FluxCuMemcpy2D;

static struct {
    int ok;
    CUresult (*ctx_push)(CUcontext);
    CUresult (*ctx_pop)(CUcontext *);
    CUresult (*ctx_get_device)(CUdevice *);
    CUresult (*device_get_uuid)(CUuuid *, CUdevice);
    CUresult (*import_ext_mem)(CUexternalMemory *, const FluxCuExtMemHandleDesc *);
    CUresult (*ext_mem_mapped_buffer)(CUdeviceptr *, CUexternalMemory, const FluxCuExtMemBufferDesc *);
    CUresult (*destroy_ext_mem)(CUexternalMemory);
    CUresult (*mem_free)(CUdeviceptr);
    CUresult (*memcpy2d_async)(const FluxCuMemcpy2D *, CUstream);
    CUresult (*stream_sync)(CUstream);
} cu;

static void cuda_load_once(void) {
    void *lib = dlopen("libcuda.so.1", RTLD_NOW | RTLD_LOCAL);
    if (!lib) return;
#define FLUX_CU_SYM(field, name) \
    *(void **)(&cu.field) = dlsym(lib, name); \
    if (!cu.field) return;
    FLUX_CU_SYM(ctx_push, "cuCtxPushCurrent_v2")
    FLUX_CU_SYM(ctx_pop, "cuCtxPopCurrent_v2")
    FLUX_CU_SYM(ctx_get_device, "cuCtxGetDevice")
    FLUX_CU_SYM(device_get_uuid, "cuDeviceGetUuid")
    FLUX_CU_SYM(import_ext_mem, "cuImportExternalMemory")
    FLUX_CU_SYM(ext_mem_mapped_buffer, "cuExternalMemoryGetMappedBuffer")
    FLUX_CU_SYM(destroy_ext_mem, "cuDestroyExternalMemory")
    FLUX_CU_SYM(mem_free, "cuMemFree_v2")
    FLUX_CU_SYM(memcpy2d_async, "cuMemcpy2DAsync_v2")
    FLUX_CU_SYM(stream_sync, "cuStreamSynchronize")
#undef FLUX_CU_SYM
    cu.ok = 1;
}

static int cuda_load(void) {
    static pthread_once_t once = PTHREAD_ONCE_INIT;
    pthread_once(&once, cuda_load_once);
    return cu.ok;
}

#define FLUX_GPU_MAX_SLOTS 8

void flux_ffmpeg_set_av_log_level(int level) {
    av_log_set_level(level);
    fprintf(stderr, "flux_ffmpeg: av_log_set_level(%d)\n", level);
}

typedef struct PktNode {
    AVPacket *pkt;
    double dur; /* video seconds carried (0 for audio) */
    struct PktNode *next;
} PktNode;

/* A decoded picture on its way to the UI: CPU bytes (RGBA or YUV planes) or a
 * CUDA surface reference (zero-copy, copied into a GPU slot at present). */
typedef struct FluxPic {
    uint8_t *buf;
    size_t cap;
    AVFrame *hw;
    int w, h, pitch, format;
    int sar_num, sar_den, matrix, full_range;
    double pts;
    int gen;
} FluxPic;

/* Vulkan memory imported into CUDA, one mapping per slot. */
typedef struct FluxGpuSlots {
    pthread_mutex_t mu; /* held across a copy, attach and detach */
    int attached;
    int refused;
    int n;
    CUexternalMemory ext[FLUX_GPU_MAX_SLOTS];
    CUdeviceptr ptr[FLUX_GPU_MAX_SLOTS];
    uint64_t slot_bytes;
    int pitch, w, h, bpc;
    /* Written under the player mutex. */
    unsigned busy;
    int last_slot;
    int need_w, need_h, need_bpc;
} FluxGpuSlots;

struct FluxFfmpegPlayer {
    pthread_t thread;
    pthread_mutex_t mu;
    int stop;
    int alive;
    int paused;
    int seek_req;
    double seek_secs;
    float volume;
    double position;
    double duration;
    double audio_clock;
    int want_hwdec;

    uint8_t *frame_rgba;
    size_t frame_cap;
    uint8_t *spare_rgba;
    size_t spare_cap;
    int frame_w;
    int frame_h;
    int frame_ready;
    /* Published picture layout (FluxFrameInfo). */
    int frame_kind;
    int frame_format;
    int frame_pitch;
    int frame_sar_num, frame_sar_den;
    int frame_matrix, frame_full_range;
    int frame_slot;
    int want_w;
    int want_h;
    int yuv_out;
    struct SwsContext *sws_yuv;
    FluxGpuSlots gpu;
    /* Last CUDA surface shown (screenshots of zero-copy playback). */
    AVFrame *last_hw;
    /* Decode fills this ring without sleeping. Present thread releases on PTS. */
    FluxPic vq[5];
    int vq_r;
    int vq_n;
    int play_gen;
    pthread_cond_t vq_cv;
    pthread_t present_thread;
    int present_started;
    /* Software frames waiting on the scale pool. Decode never scales. */
    struct {
        AVFrame *frame;
        double pts;
        int gen;
        int want_w, want_h;
    } jobs[8];
    int job_r;
    int job_n;
    pthread_cond_t job_cv;
    pthread_t scale_threads[8];
    int nscale;
    int scale_started;
    /* Present cadence (monitor refresh). Decode thread drops extras; UI never waits. */
    int present_hz;
    /* Content frame rate from the stream (24/25/30/60…). Used when PTS repeats. */
    double content_fps;
    int64_t last_present_us;
    int eof_retries;
    struct SwsContext *sws;
    AVFrame *xfer;
    int xfer_nv12; /* 10-bit surfaces: 1 GPU converts to NV12, -1 driver refused, 0 untried */
    AVBufferRef *hw_device_ctx;
    enum AVPixelFormat hw_pix_fmt;
    int use_hw;

    char *url;
    char *user_agent;
    char *referer;
    char *http_proxy;
    int low_latency;

    /* Read-ahead (ffplay read_thread / mpv demuxer cache): a demux thread reads
     * the network as fast as it delivers, decode pulls from this queue. Without
     * it the socket was only read at playback pace, so a 25 Mbit/s 4K film on a
     * ~20-30 Mbit/s link starved on every bitrate peak (micro-stutter). */
    AVFormatContext *fmt;
    int vindex;
    int aindex;
    pthread_t demux_thread;
    int demux_started;
    pthread_cond_t pq_cv;
    PktNode *pq_head;
    PktNode *pq_tail;
    size_t pq_bytes;
    double pq_secs;
    size_t pq_max_bytes;
    double pq_max_secs;
    int pq_serial;       /* bumped by the demux thread after each seek */
    double pq_flush_secs;
    int pq_eof;
    int pq_abort;
    /* Rebuffer (mpv cache-pause): queue ran dry mid-film → hold until goal. */
    int buffering;
    double buffer_goal;
    int resync_clock;    /* present reseats its film clock without dropping */
    double net_bps;
    double media_bps;
};

static void *decode_thread(void *arg);
static void *present_thread(void *arg);
static int frame_matrix(const AVFrame *f);

static char *flux_strdup(const char *s) {
    if (!s) return NULL;
    size_t n = strlen(s) + 1;
    char *d = (char *)malloc(n);
    if (d) memcpy(d, s, n);
    return d;
}

static AVCUDADeviceContext *cuda_hwctx(FluxFfmpegPlayer *p) {
    if (!p->hw_device_ctx) return NULL;
    AVHWDeviceContext *dc = (AVHWDeviceContext *)p->hw_device_ctx->data;
    return dc->type == AV_HWDEVICE_TYPE_CUDA ? (AVCUDADeviceContext *)dc->hwctx : NULL;
}

/* Caller holds gpu.mu. */
static void gpu_detach_locked(FluxFfmpegPlayer *p) {
    FluxGpuSlots *g = &p->gpu;
    AVCUDADeviceContext *hw = cuda_hwctx(p);
    CUcontext prev;
    int pushed = g->n > 0 && hw && cu.ok && cu.ctx_push(hw->cuda_ctx) == 0;
    for (int i = 0; i < g->n; i++) {
        if (pushed && g->ptr[i]) cu.mem_free(g->ptr[i]);
        if (pushed && g->ext[i]) cu.destroy_ext_mem(g->ext[i]);
        g->ptr[i] = 0;
        g->ext[i] = NULL;
    }
    if (pushed) cu.ctx_pop(&prev);
    pthread_mutex_lock(&p->mu);
    g->n = 0;
    g->attached = 0;
    g->last_slot = -1;
    pthread_mutex_unlock(&p->mu);
}

/* Next slot the UI is not reading, never the one just published (the UI may
 * not have copied it yet). -1 when all are busy. Caller holds mu. */
static int gpu_pick_slot(FluxFfmpegPlayer *p) {
    FluxGpuSlots *g = &p->gpu;
    for (int k = 1; k <= g->n; k++) {
        int s = (g->last_slot + k + g->n) % g->n;
        if (s == g->last_slot || (g->busy & (1u << s))) continue;
        return s;
    }
    return -1;
}

/* Device-to-device copy of a CUDA NV12/P010 surface into `slot`. */
static int gpu_copy(FluxFfmpegPlayer *p, const AVFrame *f, int slot) {
    FluxGpuSlots *g = &p->gpu;
    AVCUDADeviceContext *hw = cuda_hwctx(p);
    pthread_mutex_lock(&g->mu);
    int ok = hw && g->attached && slot >= 0 && slot < g->n && f->width == g->w && f->height == g->h;
    CUcontext prev;
    if (ok) ok = cu.ctx_push(hw->cuda_ctx) == 0;
    if (ok) {
        for (int plane = 0; plane < 2 && ok; plane++) {
            FluxCuMemcpy2D c;
            memset(&c, 0, sizeof(c));
            c.srcMemoryType = FLUX_CU_MEMORYTYPE_DEVICE;
            c.srcDevice = (CUdeviceptr)(uintptr_t)f->data[plane];
            c.srcPitch = (size_t)f->linesize[plane];
            c.dstMemoryType = FLUX_CU_MEMORYTYPE_DEVICE;
            c.dstDevice = g->ptr[slot] + (plane ? (CUdeviceptr)g->pitch * (CUdeviceptr)g->h : 0);
            c.dstPitch = (size_t)g->pitch;
            c.WidthInBytes = (size_t)((f->width + 1) & ~1) * (size_t)g->bpc;
            c.Height = plane ? (size_t)((f->height + 1) / 2) : (size_t)f->height;
            ok = cu.memcpy2d_async(&c, hw->stream) == 0;
        }
        /* The UI samples right after publish: the bytes must be there. */
        if (ok) ok = cu.stream_sync(hw->stream) == 0;
        cu.ctx_pop(&prev);
    }
    pthread_mutex_unlock(&g->mu);
    return ok;
}

FluxFfmpegPlayer *flux_ffmpeg_open(const FluxFfmpegOpenOpts *opts) {
    if (!opts || !opts->url || !opts->url[0]) return NULL;

    avformat_network_init();

    FluxFfmpegPlayer *p = (FluxFfmpegPlayer *)calloc(1, sizeof(*p));
    if (!p) return NULL;
    pthread_mutex_init(&p->mu, NULL);
    pthread_cond_init(&p->vq_cv, NULL);
    pthread_cond_init(&p->job_cv, NULL);
    pthread_cond_init(&p->pq_cv, NULL);
    pthread_mutex_init(&p->gpu.mu, NULL);
    p->gpu.last_slot = -1;
    p->vindex = -1;
    p->aindex = -1;
    p->url = flux_strdup(opts->url);
    p->user_agent = flux_strdup(opts->user_agent);
    p->referer = flux_strdup(opts->referer);
    p->http_proxy = flux_strdup(opts->http_proxy);
    p->low_latency = opts->low_latency;
    p->want_hwdec = opts->hwdec ? 1 : 0;
    p->volume = 1.0f;
    p->alive = 1;
    if (pthread_create(&p->present_thread, NULL, present_thread, p) != 0) {
        flux_ffmpeg_close(p);
        return NULL;
    }
    p->present_started = 1;
    p->audio_clock = NAN;
    p->want_w = 1920;
    p->want_h = 1080;
    p->present_hz = 60;
    p->content_fps = 30.0;
    p->eof_retries = 0;

    if (pthread_create(&p->thread, NULL, decode_thread, p) != 0) {
        flux_ffmpeg_close(p);
        return NULL;
    }
    return p;
}

void flux_ffmpeg_close(FluxFfmpegPlayer *p) {
    if (!p) return;
    pthread_mutex_lock(&p->mu);
    p->stop = 1;
    pthread_cond_broadcast(&p->vq_cv);
    pthread_mutex_unlock(&p->mu);
    if (p->present_started) {
        pthread_join(p->present_thread, NULL);
        p->present_started = 0;
    }
    if (p->thread) {
        pthread_join(p->thread, NULL);
        p->thread = 0;
    }
    free(p->frame_rgba);
    free(p->spare_rgba);
    for (int i = 0; i < 5; i++) {
        free(p->vq[i].buf);
        if (p->vq[i].hw) av_frame_free(&p->vq[i].hw);
    }
    if (p->last_hw) av_frame_free(&p->last_hw);
    for (int i = 0; i < 8; i++) {
        if (p->jobs[i].frame) av_frame_free(&p->jobs[i].frame);
    }
    pthread_cond_destroy(&p->vq_cv);
    pthread_cond_destroy(&p->job_cv);
    pthread_cond_destroy(&p->pq_cv);
    if (p->sws) sws_freeContext(p->sws);
    if (p->sws_yuv) sws_freeContext(p->sws_yuv);
    if (p->xfer) av_frame_free(&p->xfer);
    /* Imports live in the CUDA context: release them before it goes. */
    pthread_mutex_lock(&p->gpu.mu);
    gpu_detach_locked(p);
    pthread_mutex_unlock(&p->gpu.mu);
    pthread_mutex_destroy(&p->gpu.mu);
    if (p->hw_device_ctx) av_buffer_unref(&p->hw_device_ctx);
    free(p->url);
    free(p->user_agent);
    free(p->referer);
    free(p->http_proxy);
    pthread_mutex_destroy(&p->mu);
    free(p);
}

int flux_ffmpeg_pull_rgba(FluxFfmpegPlayer *p, uint8_t *out, int out_w, int out_h, int *got_w, int *got_h) {
    if (!p || !out || out_w < 2 || out_h < 2) return 0;
    pthread_mutex_lock(&p->mu);
    p->want_w = out_w > 3840 ? 3840 : out_w;
    p->want_h = out_h > 2160 ? 2160 : out_h;
    if (!p->frame_ready || !p->frame_rgba || p->frame_format != FLUX_FMT_RGBA || p->frame_w < 2
        || p->frame_h < 2) {
        pthread_mutex_unlock(&p->mu);
        return 0;
    }
    /* Never memcpy past the caller buffer. Caller must size out via frame_size(). */
    int fw = p->frame_w;
    int fh = p->frame_h;
    size_t need = (size_t)fw * (size_t)fh * 4;
    size_t out_bytes = (size_t)out_w * (size_t)out_h * 4;
    if (need == 0 || need > out_bytes) {
        /* Keep the frame ready — Rust will retry with a larger buffer. */
        pthread_mutex_unlock(&p->mu);
        return 0;
    }
    uint8_t *src = p->frame_rgba;
    /* Hand the buffer off so the copy does not hold the lock (present keeps pacing). */
    p->frame_rgba = NULL;
    p->frame_cap = 0;
    p->frame_ready = 0;
    pthread_cond_signal(&p->vq_cv);
    pthread_mutex_unlock(&p->mu);
    memcpy(out, src, (size_t)fw * (size_t)fh * 4);
    pthread_mutex_lock(&p->mu);
    if (!p->spare_rgba) {
        p->spare_rgba = src;
        p->spare_cap = (size_t)fw * (size_t)fh * 4;
    } else {
        free(src);
    }
    pthread_mutex_unlock(&p->mu);
    if (got_w) *got_w = fw;
    if (got_h) *got_h = fh;
    return 1;
}

int flux_ffmpeg_frame_size(FluxFfmpegPlayer *p, int *w, int *h) {
    if (!p) return 0;
    pthread_mutex_lock(&p->mu);
    int ok = p->frame_ready && p->frame_rgba && p->frame_format == FLUX_FMT_RGBA && p->frame_w >= 2
        && p->frame_h >= 2;
    if (ok) {
        if (w) *w = p->frame_w;
        if (h) *h = p->frame_h;
    }
    pthread_mutex_unlock(&p->mu);
    return ok ? 1 : 0;
}

void flux_ffmpeg_set_yuv_output(FluxFfmpegPlayer *p, int on) {
    if (!p) return;
    pthread_mutex_lock(&p->mu);
    p->yuv_out = on ? 1 : 0;
    pthread_mutex_unlock(&p->mu);
}

/* Caller holds mu. */
static int frame_info_locked(FluxFfmpegPlayer *p, FluxFrameInfo *info) {
    int ready = p->frame_ready && p->frame_w >= 2 && p->frame_h >= 2
        && (p->frame_kind == FLUX_FRAME_GPU || p->frame_rgba);
    if (!ready || !info) return ready;
    memset(info, 0, sizeof(*info));
    info->kind = p->frame_kind;
    info->format = p->frame_format;
    info->width = p->frame_w;
    info->height = p->frame_h;
    info->pitch = p->frame_pitch;
    info->sar_num = p->frame_sar_num;
    info->sar_den = p->frame_sar_den;
    info->matrix = p->frame_matrix;
    info->full_range = p->frame_full_range;
    info->slot = p->frame_slot;
    if (p->frame_kind == FLUX_FRAME_CPU) {
        size_t rows = p->frame_format == FLUX_FMT_RGBA
            ? (size_t)p->frame_h
            : (size_t)p->frame_h + (size_t)(p->frame_h + 1) / 2;
        info->bytes = (uint64_t)rows * (uint64_t)p->frame_pitch;
    }
    return 1;
}

int flux_ffmpeg_frame_info(FluxFfmpegPlayer *p, FluxFrameInfo *info) {
    if (!p) return 0;
    pthread_mutex_lock(&p->mu);
    int ok = frame_info_locked(p, info);
    pthread_mutex_unlock(&p->mu);
    return ok;
}

int flux_ffmpeg_pull_frame(FluxFfmpegPlayer *p, FluxFrameInfo *info, uint8_t *out, uint64_t cap) {
    if (!p || !info) return 0;
    pthread_mutex_lock(&p->mu);
    if (!frame_info_locked(p, info)) {
        pthread_mutex_unlock(&p->mu);
        return 0;
    }
    if (info->kind == FLUX_FRAME_GPU) {
        /* Busy until the UI reports its copy done (gpu_release). */
        if (info->slot >= 0) p->gpu.busy |= 1u << info->slot;
        p->frame_ready = 0;
        pthread_cond_signal(&p->vq_cv);
        pthread_mutex_unlock(&p->mu);
        return 1;
    }
    if (!out || cap < info->bytes) {
        pthread_mutex_unlock(&p->mu);
        return 0;
    }
    uint8_t *src = p->frame_rgba;
    size_t src_cap = p->frame_cap;
    p->frame_rgba = NULL;
    p->frame_cap = 0;
    p->frame_ready = 0;
    pthread_cond_signal(&p->vq_cv);
    pthread_mutex_unlock(&p->mu);
    memcpy(out, src, (size_t)info->bytes);
    pthread_mutex_lock(&p->mu);
    if (!p->spare_rgba) {
        p->spare_rgba = src;
        p->spare_cap = src_cap;
    } else {
        free(src);
    }
    pthread_mutex_unlock(&p->mu);
    return 1;
}

int flux_ffmpeg_gpu_need(FluxFfmpegPlayer *p, int *w, int *h, int *bpc) {
    if (!p) return 0;
    pthread_mutex_lock(&p->mu);
    FluxGpuSlots *g = &p->gpu;
    int need = p->yuv_out && !g->refused && g->need_w > 0
        && !(g->attached && g->w == g->need_w && g->h == g->need_h && g->bpc == g->need_bpc);
    if (need) {
        if (w) *w = g->need_w;
        if (h) *h = g->need_h;
        if (bpc) *bpc = g->need_bpc;
    }
    pthread_mutex_unlock(&p->mu);
    return need;
}

int flux_ffmpeg_gpu_attach(FluxFfmpegPlayer *p, const int *fds, int n, uint64_t slot_bytes,
                           int pitch, int w, int h, int bpc, const uint8_t *uuid) {
    int ok = 0;
    int taken = 0;
    if (!p || !fds || n < 2 || n > FLUX_GPU_MAX_SLOTS || !uuid || w < 2 || h < 2 || (bpc != 1 && bpc != 2)
        || pitch < ((w + 1) & ~1) * bpc
        || slot_bytes < (uint64_t)pitch * (uint64_t)(h + (h + 1) / 2))
        goto out;
    FluxGpuSlots *g = &p->gpu;
    pthread_mutex_lock(&g->mu);
    AVCUDADeviceContext *hw = cuda_hwctx(p);
    CUcontext prev;
    if (!hw || !cuda_load() || cu.ctx_push(hw->cuda_ctx) != 0) {
        pthread_mutex_unlock(&g->mu);
        goto out;
    }
    CUdevice dev = 0;
    CUuuid id;
    if (cu.ctx_get_device(&dev) != 0 || cu.device_get_uuid(&id, dev) != 0
        || memcmp(id.bytes, uuid, 16) != 0) {
        fprintf(stderr, "flux_ffmpeg: GPU stage on another device — frames go through the CPU\n");
        cu.ctx_pop(&prev);
        pthread_mutex_unlock(&g->mu);
        goto out;
    }
    gpu_detach_locked(p);
    CUexternalMemory ext[FLUX_GPU_MAX_SLOTS] = {0};
    CUdeviceptr ptr[FLUX_GPU_MAX_SLOTS] = {0};
    int i = 0;
    for (; i < n; i++) {
        FluxCuExtMemHandleDesc hd;
        memset(&hd, 0, sizeof(hd));
        hd.type = FLUX_CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD;
        hd.handle.fd = fds[i];
        hd.size = slot_bytes;
        if (cu.import_ext_mem(&ext[i], &hd) != 0) break;
        taken = i + 1; /* CUDA owns the fd once imported */
        FluxCuExtMemBufferDesc bd;
        memset(&bd, 0, sizeof(bd));
        bd.size = slot_bytes;
        if (cu.ext_mem_mapped_buffer(&ptr[i], ext[i], &bd) != 0) {
            i++;
            break;
        }
    }
    ok = taken == n && ptr[n - 1] != 0;
    if (!ok) {
        for (int k = 0; k < n; k++) {
            if (ptr[k]) cu.mem_free(ptr[k]);
            if (ext[k]) cu.destroy_ext_mem(ext[k]);
        }
    }
    cu.ctx_pop(&prev);
    if (ok) {
        pthread_mutex_lock(&p->mu);
        for (int k = 0; k < n; k++) {
            g->ext[k] = ext[k];
            g->ptr[k] = ptr[k];
        }
        g->n = n;
        g->slot_bytes = slot_bytes;
        g->pitch = pitch;
        g->w = w;
        g->h = h;
        g->bpc = bpc;
        g->busy = 0;
        g->last_slot = -1;
        g->attached = 1;
        /* Its slot index named the old set. */
        if (p->frame_kind == FLUX_FRAME_GPU) p->frame_ready = 0;
        pthread_mutex_unlock(&p->mu);
        fprintf(stderr, "flux_ffmpeg: zero-copy GPU stage (%d slots, %dx%d, %d-bit)\n", n, w, h,
                bpc == 2 ? 10 : 8);
    }
    pthread_mutex_unlock(&g->mu);
out:
    if (fds) {
        for (int k = taken; k < n; k++) {
            if (fds[k] >= 0) close(fds[k]);
        }
    }
    if (!ok && p) {
        pthread_mutex_lock(&p->mu);
        p->gpu.refused = 1;
        pthread_mutex_unlock(&p->mu);
    }
    return ok;
}

void flux_ffmpeg_gpu_release(FluxFfmpegPlayer *p, int slot) {
    if (!p || slot < 0 || slot >= FLUX_GPU_MAX_SLOTS) return;
    pthread_mutex_lock(&p->mu);
    p->gpu.busy &= ~(1u << slot);
    pthread_mutex_unlock(&p->mu);
}

int flux_ffmpeg_gpu_snapshot(FluxFfmpegPlayer *p, uint8_t *out, uint64_t cap, int *w, int *h) {
    if (!p || !out) return 0;
    pthread_mutex_lock(&p->mu);
    AVFrame *ref = p->last_hw ? av_frame_clone(p->last_hw) : NULL;
    pthread_mutex_unlock(&p->mu);
    if (!ref) return 0;
    int ok = 0;
    AVFrame *sw = av_frame_alloc();
    if (sw && av_hwframe_transfer_data(sw, ref, 0) >= 0
        && (uint64_t)sw->width * (uint64_t)sw->height * 4 <= cap) {
        struct SwsContext *s = sws_getContext(sw->width, sw->height, sw->format, sw->width, sw->height,
                                              AV_PIX_FMT_RGBA, SWS_POINT, NULL, NULL, NULL);
        if (s) {
            int m = frame_matrix(ref);
            int cs = m == FLUX_MATRIX_BT2020 ? SWS_CS_BT2020 : m == FLUX_MATRIX_BT709 ? SWS_CS_ITU709 : SWS_CS_ITU601;
            sws_setColorspaceDetails(s, sws_getCoefficients(cs), ref->color_range == AVCOL_RANGE_JPEG,
                                     sws_getCoefficients(SWS_CS_DEFAULT), 1, 0, 1 << 16, 1 << 16);
            uint8_t *dst[4] = {out, NULL, NULL, NULL};
            int stride[4] = {sw->width * 4, 0, 0, 0};
            sws_scale(s, (const uint8_t *const *)sw->data, sw->linesize, 0, sw->height, dst, stride);
            sws_freeContext(s);
            if (w) *w = sw->width;
            if (h) *h = sw->height;
            ok = 1;
        }
    }
    av_frame_free(&sw);
    av_frame_free(&ref);
    return ok;
}

void flux_ffmpeg_set_output_size(FluxFfmpegPlayer *p, int w, int h) {
    if (!p || w < 2 || h < 2) return;
    pthread_mutex_lock(&p->mu);
    p->want_w = w > 3840 ? 3840 : w;
    p->want_h = h > 2160 ? 2160 : h;
    pthread_mutex_unlock(&p->mu);
}

void flux_ffmpeg_set_present_hz(FluxFfmpegPlayer *p, int hz) {
    if (!p) return;
    if (hz < 24) hz = 24;
    if (hz > 120) hz = 120;
    pthread_mutex_lock(&p->mu);
    p->present_hz = hz;
    pthread_mutex_unlock(&p->mu);
}

int flux_ffmpeg_is_alive(FluxFfmpegPlayer *p) {
    if (!p) return 0;
    pthread_mutex_lock(&p->mu);
    /* Stay alive while pictures remain after decode EOF — otherwise the UI
     * sees death before the last frames are pulled (0-frame smoke / freeze). */
    int a = !p->stop && (p->alive || p->frame_ready || p->vq_n > 0);
    pthread_mutex_unlock(&p->mu);
    return a;
}

int flux_ffmpeg_has_frame(FluxFfmpegPlayer *p) {
    if (!p) return 0;
    pthread_mutex_lock(&p->mu);
    /* Only report ready when dims match want — avoids pull→mismatch spin.
     * Decode skip_store also keys on size match so retargets still store. */
    /* Ready when the stored frame fits the request. Native size may be
     * smaller than the window (we do not upscale). */
    int r = frame_info_locked(p, NULL);
    pthread_mutex_unlock(&p->mu);
    return r;
}

void flux_ffmpeg_pause(FluxFfmpegPlayer *p, int paused) {
    if (!p) return;
    pthread_mutex_lock(&p->mu);
    p->paused = paused ? 1 : 0;
    pthread_mutex_unlock(&p->mu);
}

void flux_ffmpeg_set_volume(FluxFfmpegPlayer *p, float volume01) {
    if (!p) return;
    if (volume01 < 0.f) volume01 = 0.f;
    if (volume01 > 1.f) volume01 = 1.f;
    pthread_mutex_lock(&p->mu);
    p->volume = volume01;
    pthread_mutex_unlock(&p->mu);
}

double flux_ffmpeg_position_secs(FluxFfmpegPlayer *p) {
    if (!p) return 0.0;
    pthread_mutex_lock(&p->mu);
    double v = p->position;
    pthread_mutex_unlock(&p->mu);
    return v;
}

double flux_ffmpeg_duration_secs(FluxFfmpegPlayer *p) {
    if (!p) return 0.0;
    pthread_mutex_lock(&p->mu);
    double v = p->duration;
    pthread_mutex_unlock(&p->mu);
    return v;
}

void flux_ffmpeg_seek(FluxFfmpegPlayer *p, double secs) {
    if (!p) return;
    pthread_mutex_lock(&p->mu);
    p->seek_req = 1;
    p->seek_secs = secs < 0 ? 0 : secs;
    pthread_mutex_unlock(&p->mu);
}

int flux_ffmpeg_buffer_state(FluxFfmpegPlayer *p, double *buffered_secs, double *goal_secs,
                             double *net_mbps, double *media_mbps) {
    if (!p) return 0;
    pthread_mutex_lock(&p->mu);
    int b = p->buffering;
    if (buffered_secs) *buffered_secs = p->pq_secs;
    if (goal_secs) *goal_secs = p->buffer_goal;
    if (net_mbps) *net_mbps = p->net_bps / 1e6;
    if (media_mbps) *media_mbps = p->media_bps / 1e6;
    pthread_mutex_unlock(&p->mu);
    return b;
}

static int interrupted(void *opaque) {
    FluxFfmpegPlayer *p = (FluxFfmpegPlayer *)opaque;
    int s;
    pthread_mutex_lock(&p->mu);
    s = p->stop || p->pq_abort;
    pthread_mutex_unlock(&p->mu);
    return s;
}

static void timed_wait_ms(pthread_cond_t *cv, pthread_mutex_t *mu, long ms) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    ts.tv_nsec += ms * 1000000L;
    while (ts.tv_nsec >= 1000000000L) {
        ts.tv_sec += 1;
        ts.tv_nsec -= 1000000000L;
    }
    pthread_cond_timedwait(cv, mu, &ts);
}

/* Caller holds mu. */
static void pq_clear(FluxFfmpegPlayer *p) {
    PktNode *n = p->pq_head;
    while (n) {
        PktNode *next = n->next;
        av_packet_free(&n->pkt);
        free(n);
        n = next;
    }
    p->pq_head = p->pq_tail = NULL;
    p->pq_bytes = 0;
    p->pq_secs = 0.0;
}

/* Caller holds mu. */
static PktNode *pq_pop(FluxFfmpegPlayer *p) {
    PktNode *n = p->pq_head;
    if (!n) return NULL;
    p->pq_head = n->next;
    if (!p->pq_head) p->pq_tail = NULL;
    size_t sz = (size_t)n->pkt->size + sizeof(*n);
    p->pq_bytes = p->pq_bytes > sz ? p->pq_bytes - sz : 0;
    p->pq_secs -= n->dur;
    if (p->pq_secs < 0.0 || !p->pq_head) p->pq_secs = 0.0;
    pthread_cond_broadcast(&p->pq_cv);
    return n;
}

/* Owns every call on p->fmt once playback starts (lavf is not thread-safe):
 * reads, live EOF retries and seeks. */
static void *demux_thread(void *arg) {
    FluxFfmpegPlayer *p = (FluxFfmpegPlayer *)arg;
    AVFormatContext *fmt = p->fmt;
    AVPacket *pkt = av_packet_alloc();
    double vtb = av_q2d(fmt->streams[p->vindex]->time_base);
    int64_t win_t = 0, win_b = 0;
    if (!pkt) {
        pthread_mutex_lock(&p->mu);
        p->pq_eof = 1;
        pthread_cond_broadcast(&p->pq_cv);
        pthread_mutex_unlock(&p->mu);
        return NULL;
    }
    pthread_mutex_lock(&p->mu);
    if (fmt->bit_rate > 0) p->media_bps = (double)fmt->bit_rate;
    pthread_mutex_unlock(&p->mu);
    for (;;) {
        pthread_mutex_lock(&p->mu);
        if (p->stop || p->pq_abort) {
            pthread_mutex_unlock(&p->mu);
            break;
        }
        int seek_req = p->seek_req;
        double seek_secs = p->seek_secs;
        if (seek_req) p->seek_req = 0;
        int full = p->pq_bytes >= p->pq_max_bytes || p->pq_secs >= p->pq_max_secs;
        if (!seek_req && (full || p->pq_eof)) {
            timed_wait_ms(&p->pq_cv, &p->mu, 20);
            pthread_mutex_unlock(&p->mu);
            win_t = 0; /* idle by choice: not a network measurement */
            continue;
        }
        double fps = p->content_fps >= 20.0 ? p->content_fps : 30.0;
        pthread_mutex_unlock(&p->mu);

        if (seek_req) {
            int64_t ts = (int64_t)(seek_secs * AV_TIME_BASE);
            if (av_seek_frame(fmt, -1, ts, AVSEEK_FLAG_BACKWARD) >= 0) {
                pthread_mutex_lock(&p->mu);
                pq_clear(p);
                p->pq_serial++;
                p->pq_flush_secs = seek_secs;
                p->pq_eof = 0;
                pthread_cond_broadcast(&p->pq_cv);
                pthread_mutex_unlock(&p->mu);
            }
            win_t = 0;
            continue;
        }

        int r = av_read_frame(fmt, pkt);
        if (r < 0) {
            if (r == AVERROR_EOF) {
                /* Live/HLS: lavf often returns EOF between playlist reloads — retry
                 * briefly. Hard-capped so a duration≤0 VOD cannot spin forever. */
                int live = fmt->duration <= 0;
                pthread_mutex_lock(&p->mu);
                if (live && !p->stop && p->eof_retries++ < 120) { /* ~30s at 250ms */
                    pthread_mutex_unlock(&p->mu);
                    usleep(250000);
                    continue;
                }
                p->pq_eof = 1;
                pthread_cond_broadcast(&p->pq_cv);
                pthread_mutex_unlock(&p->mu);
                continue;
            }
            usleep(10000);
            continue;
        }
        if (pkt->stream_index != p->vindex && pkt->stream_index != p->aindex) {
            av_packet_unref(pkt);
            continue;
        }
        PktNode *n = (PktNode *)calloc(1, sizeof(*n));
        if (n) n->pkt = av_packet_alloc();
        if (!n || !n->pkt) {
            free(n);
            av_packet_unref(pkt);
            usleep(10000);
            continue;
        }
        if (pkt->stream_index == p->vindex)
            n->dur = pkt->duration > 0 ? (double)pkt->duration * vtb : 1.0 / fps;
        av_packet_move_ref(n->pkt, pkt);

        int64_t now = av_gettime_relative();
        int64_t bytes = fmt->pb ? fmt->pb->bytes_read : 0;
        pthread_mutex_lock(&p->mu);
        if (p->pq_tail) p->pq_tail->next = n;
        else p->pq_head = n;
        p->pq_tail = n;
        p->pq_bytes += (size_t)n->pkt->size + sizeof(*n);
        p->pq_secs += n->dur;
        if (!win_t) {
            win_t = now;
            win_b = bytes;
        } else if (now - win_t >= 2000000 && bytes > win_b) {
            double bps = (double)(bytes - win_b) * 8e6 / (double)(now - win_t);
            p->net_bps = p->net_bps > 0.0 ? 0.6 * p->net_bps + 0.4 * bps : bps;
            win_t = now;
            win_b = bytes;
        }
        pthread_cond_broadcast(&p->pq_cv);
        pthread_mutex_unlock(&p->mu);
    }
    av_packet_free(&pkt);
    return NULL;
}

static int store_frame(struct SwsContext **sws_io, AVFrame *frame, int target_w, int target_h,
                       uint8_t **rgba, size_t *cap, int *out_w, int *out_h) {
    if (!sws_io || !rgba || !cap || !frame || frame->width < 2 || frame->height < 2) return 0;
    int tw = target_w > 0 ? target_w : frame->width;
    int th = target_h > 0 ? target_h : frame->height;
    if (tw > 3840) tw = 3840;
    if (th > 2160) th = 2160;
    /* Scale here, on the decode thread, into the monitor-sized target.
     * The UI only uploads the finished buffer. */
    if (tw < 2) tw = 2;
    if (th < 2) th = 2;

    /* Aspect-preserving fit into want WxH (letterbox / pillarbox), never stretch.
     * Honour sample aspect ratio so anamorphic streams keep correct DAR. */
    double sar = 1.0;
    if (frame->sample_aspect_ratio.num > 0 && frame->sample_aspect_ratio.den > 0) {
        sar = (double)frame->sample_aspect_ratio.num / (double)frame->sample_aspect_ratio.den;
    }
    double disp_w = (double)frame->width * sar;
    double disp_h = (double)frame->height;
    double sx = (double)tw / disp_w;
    double sy = (double)th / disp_h;
    double s = sx < sy ? sx : sy;
    int dw = (int)(disp_w * s + 0.5);
    int dh = (int)(disp_h * s + 0.5);
    if (dw < 2) dw = 2;
    if (dh < 2) dh = 2;
    dw &= ~1;
    dh &= ~1;
    if (dw > tw) dw = tw & ~1;
    if (dh > th) dh = th & ~1;
    int ox = ((tw - dw) / 2) & ~1;
    int oy = ((th - dh) / 2) & ~1;

    /* Upscale: bicubic. Modest shrink: area. Big shrink (4K→panel): fast bilinear
     * so one frame still lands inside 41 ms — area on full UHD was the hitch. */
    double shrink = (double)frame->width / (double)(dw > 0 ? dw : 1);
    int sws_flags = (dw > frame->width || dh > frame->height) ? SWS_BICUBIC
        : (shrink > 2.0 ? SWS_FAST_BILINEAR : SWS_AREA);
    static pthread_mutex_t sws_init_mu = PTHREAD_MUTEX_INITIALIZER;
    pthread_mutex_lock(&sws_init_mu);
    struct SwsContext *sws = *sws_io;
    int reuse = 0;
    if (sws) {
        int64_t ow = 0, oh = 0, ofmt = 0, odw = 0, odh = 0, ofl = 0;
        av_opt_get_int(sws, "srcw", 0, &ow);
        av_opt_get_int(sws, "srch", 0, &oh);
        av_opt_get_int(sws, "src_format", 0, &ofmt);
        av_opt_get_int(sws, "dstw", 0, &odw);
        av_opt_get_int(sws, "dsth", 0, &odh);
        av_opt_get_int(sws, "sws_flags", 0, &ofl);
        reuse = ow == frame->width && oh == frame->height && ofmt == frame->format
            && odw == dw && odh == dh && ofl == sws_flags;
    }
    if (!reuse) {
        if (sws) sws_freeContext(sws);
        sws = sws_alloc_context();
        if (sws) {
            long ncpu = sysconf(_SC_NPROCESSORS_ONLN);
            if (ncpu < 1) ncpu = 1;
            if (ncpu > 6) ncpu = 6;
            av_opt_set_int(sws, "srcw", frame->width, 0);
            av_opt_set_int(sws, "srch", frame->height, 0);
            av_opt_set_int(sws, "src_format", frame->format, 0);
            av_opt_set_int(sws, "dstw", dw, 0);
            av_opt_set_int(sws, "dsth", dh, 0);
            av_opt_set_int(sws, "dst_format", AV_PIX_FMT_RGBA, 0);
            av_opt_set_int(sws, "sws_flags", sws_flags, 0);
            av_opt_set_int(sws, "threads", ncpu, 0);
            if (sws_init_context(sws, NULL, NULL) < 0) {
                sws_freeContext(sws);
                sws = NULL;
            }
        }
    }
    pthread_mutex_unlock(&sws_init_mu);
    if (!sws) return 0;
    *sws_io = sws;
    /* Match the stream matrix (HD vs SD). Wrong matrix looks soft and grey. */
    int cs = SWS_CS_ITU709;
    if (frame->colorspace == AVCOL_SPC_BT470BG || frame->colorspace == AVCOL_SPC_SMPTE170M)
        cs = SWS_CS_ITU601;
    int range = frame->color_range == AVCOL_RANGE_JPEG;
    /* Rebuilding the matrix every frame hitching the scale. Once per change. */
    static int last_cs = -1, last_range = -1;
    static struct SwsContext *last_sws;
    if (cs != last_cs || range != last_range || sws != last_sws) {
        sws_setColorspaceDetails(
            sws,
            sws_getCoefficients(cs), range,
            sws_getCoefficients(cs), 0,
            0, 1 << 16, 1 << 16);
        last_cs = cs;
        last_range = range;
        last_sws = sws;
    }

    size_t need = (size_t)tw * (size_t)th * 4;
    uint8_t *dst = *rgba;
    if (!dst || *cap < need) {
        free(dst);
        dst = (uint8_t *)malloc(need);
        if (!dst) {
            *rgba = NULL;
            *cap = 0;
            return 0;
        }
        *rgba = dst;
        *cap = need;
    }
    if (ox != 0 || oy != 0 || dw != tw || dh != th) {
        /* Clear only the bars, not the whole picture, before the scale writes. */
        int stride = tw * 4;
        if (oy > 0) memset(dst, 0, (size_t)oy * (size_t)stride);
        int bot = th - oy - dh;
        if (bot > 0) memset(dst + (size_t)(oy + dh) * (size_t)stride, 0, (size_t)bot * (size_t)stride);
        if (ox > 0 || dw < tw) {
            for (int y = oy; y < oy + dh; y++) {
                uint8_t *row = dst + (size_t)y * (size_t)stride;
                if (ox > 0) memset(row, 0, (size_t)ox * 4);
                int right = tw - ox - dw;
                if (right > 0) memset(row + (size_t)(ox + dw) * 4, 0, (size_t)right * 4);
            }
        }
    }
    uint8_t *dst_slices[4] = {
        dst + (size_t)oy * (size_t)tw * 4 + (size_t)ox * 4, NULL, NULL, NULL};
    int dst_stride[4] = {tw * 4, 0, 0, 0};
    sws_scale(sws, (const uint8_t *const *)frame->data, frame->linesize, 0, frame->height, dst_slices, dst_stride);
    if (out_w) *out_w = tw;
    if (out_h) *out_h = th;
    return 1;
}

static int frame_matrix(const AVFrame *f) {
    switch (f->colorspace) {
    case AVCOL_SPC_BT709:
        return FLUX_MATRIX_BT709;
    case AVCOL_SPC_BT470BG:
    case AVCOL_SPC_SMPTE170M:
        return FLUX_MATRIX_BT601;
    case AVCOL_SPC_BT2020_NCL:
    case AVCOL_SPC_BT2020_CL:
        return FLUX_MATRIX_BT2020;
    default:
        return f->height >= 720 ? FLUX_MATRIX_BT709 : FLUX_MATRIX_BT601;
    }
}

/* Decoded planes as NV12 / P010 at decoded size into `*buf` (Y rows then UV
 * rows, `pic->pitch` bytes each). Other layouts go through one unscaled sws
 * pass. The shader does colour conversion and scaling. */
static int store_yuv(struct SwsContext **sws_io, AVFrame *f, uint8_t **buf, size_t *cap, FluxPic *pic) {
    if (!f || f->width < 2 || f->height < 2) return 0;
    const AVPixFmtDescriptor *d = av_pix_fmt_desc_get(f->format);
    if (!d) return 0;
    int deep = d->comp[0].depth > 8;
    enum AVPixelFormat want = deep ? AV_PIX_FMT_P010 : AV_PIX_FMT_NV12;
    int bpc = deep ? 2 : 1;
    int w = f->width, h = f->height;
    int pitch = ((w + 1) & ~1) * bpc;
    int ch = (h + 1) / 2;
    size_t need = (size_t)pitch * (size_t)(h + ch);
    if (!*buf || *cap < need) {
        free(*buf);
        *buf = (uint8_t *)malloc(need);
        *cap = *buf ? need : 0;
        if (!*buf) return 0;
    }
    uint8_t *y = *buf;
    uint8_t *uv = *buf + (size_t)pitch * (size_t)h;
    if (f->format == want) {
        av_image_copy_plane(y, pitch, f->data[0], f->linesize[0], w * bpc, h);
        av_image_copy_plane(uv, pitch, f->data[1], f->linesize[1], ((w + 1) & ~1) * bpc, ch);
    } else {
        struct SwsContext *s = sws_getCachedContext(*sws_io, w, h, f->format, w, h, want, SWS_POINT,
                                                    NULL, NULL, NULL);
        if (!s) return 0;
        *sws_io = s;
        uint8_t *dst[4] = {y, uv, NULL, NULL};
        int stride[4] = {pitch, pitch, 0, 0};
        sws_scale(s, (const uint8_t *const *)f->data, f->linesize, 0, h, dst, stride);
    }
    pic->w = w;
    pic->h = h;
    pic->pitch = pitch;
    pic->format = deep ? FLUX_FMT_P010 : FLUX_FMT_NV12;
    return 1;
}

/* Give a picture's resources back (caller holds mu): CPU bytes to the spare
 * slot, CUDA surfaces to the decoder pool. */
static void pic_release_locked(FluxFfmpegPlayer *p, FluxPic *pic) {
    if (pic->buf) {
        if (!p->spare_rgba) {
            p->spare_rgba = pic->buf;
            p->spare_cap = pic->cap;
        } else {
            free(pic->buf);
        }
    }
    if (pic->hw) av_frame_free(&pic->hw);
    pic->buf = NULL;
    pic->cap = 0;
}

/* Sleep until an absolute CLOCK_MONOTONIC timestamp. usleep overshoots and
 * the next frame shortens to catch up — that alternation is the judder. */
static void sleep_until_us(int64_t target_us) {
    struct timespec ts;
    if (target_us < 0) target_us = 0;
    ts.tv_sec = (time_t)(target_us / 1000000);
    ts.tv_nsec = (long)((target_us % 1000000) * 1000);
    while (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &ts, NULL) == EINTR) {
    }
}

/* Drop the head of the present queue without publishing (caller holds mu). */
static void discard_vq_head(FluxFfmpegPlayer *p) {
    if (p->vq_n <= 0) return;
    pic_release_locked(p, &p->vq[p->vq_r]);
    p->vq_r = (p->vq_r + 1) % 5;
    p->vq_n--;
    pthread_cond_signal(&p->vq_cv);
}

/* Queue a decoded picture (takes ownership; caller holds mu). Back-pressure
 * when the present queue is full so decode cannot race ahead of the film
 * clock (that made every frame "late" → black screen while audio kept
 * realtime pace). Present already overwrites unread UI pixels, so this wait
 * cannot deadlock with the soft pump. */
static void vq_push(FluxFfmpegPlayer *p, FluxPic *pic) {
    if (p->stop || pic->gen != p->play_gen) {
        pic_release_locked(p, pic);
        return;
    }
    while (p->vq_n >= 4 && !p->stop && pic->gen == p->play_gen) {
        struct timespec ts;
        clock_gettime(CLOCK_REALTIME, &ts);
        ts.tv_nsec += 40000000L;
        if (ts.tv_nsec >= 1000000000L) {
            ts.tv_sec += 1;
            ts.tv_nsec -= 1000000000L;
        }
        pthread_cond_timedwait(&p->vq_cv, &p->mu, &ts);
    }
    if (p->stop || pic->gen != p->play_gen) {
        pic_release_locked(p, pic);
        return;
    }
    if (p->vq_n == 5) discard_vq_head(p);
    int slot = (p->vq_r + p->vq_n) % 5;
    pic_release_locked(p, &p->vq[slot]);
    p->vq[slot] = *pic;
    pic->buf = NULL;
    pic->cap = 0;
    pic->hw = NULL;
    p->vq_n++;
    pthread_cond_signal(&p->vq_cv);
}

/* Release decoded frames paced by the audio clock (master) or film PTS.
 * Late frames are dropped — never dumped in a catch-up burst (speed wobble). */
static void *present_thread(void *arg) {
    FluxFfmpegPlayer *p = (FluxFfmpegPlayer *)arg;
    struct sched_param sp;
    memset(&sp, 0, sizeof(sp));
    sp.sched_priority = 10;
    /* Best effort: stay on the refresh grid when the CPU is busy copying frames. */
    (void)pthread_setschedparam(pthread_self(), SCHED_FIFO, &sp);
    int64_t clock0 = 0;
    double pts0 = NAN;
    int gen0 = -1;
    int64_t last_pub_us = 0;
    while (1) {
        pthread_mutex_lock(&p->mu);
        /* Wait only for a queued picture. Never wait on an unread UI frame —
         * that filled the VO queue, blocked decode, and silenced audio (freeze).
         * Overwrite unread pixels like mpv framedrop=vo. */
        while (p->vq_n == 0 && !p->stop) {
            struct timespec ts;
            clock_gettime(CLOCK_REALTIME, &ts);
            ts.tv_nsec += 50000000L;
            if (ts.tv_nsec >= 1000000000L) {
                ts.tv_sec += 1;
                ts.tv_nsec -= 1000000000L;
            }
            pthread_cond_timedwait(&p->vq_cv, &p->mu, &ts);
        }
        if (p->stop && p->vq_n == 0) {
            pthread_mutex_unlock(&p->mu);
            break;
        }
        if (p->vq_n == 0) {
            pthread_mutex_unlock(&p->mu);
            continue;
        }
        /* Brief preroll only — do not stall the open for hundreds of ms. */
        if (p->vq[p->vq_r].gen != gen0 && p->vq_n < 2) {
            struct timespec ts;
            clock_gettime(CLOCK_REALTIME, &ts);
            ts.tv_nsec += 80000000L;
            if (ts.tv_nsec >= 1000000000L) {
                ts.tv_sec += 1;
                ts.tv_nsec -= 1000000000L;
            }
            while (p->vq_n < 2 && !p->stop) {
                if (pthread_cond_timedwait(&p->vq_cv, &p->mu, &ts) == ETIMEDOUT) break;
            }
            if (p->stop && p->vq_n == 0) {
                pthread_mutex_unlock(&p->mu);
                break;
            }
            if (p->vq_n == 0) {
                pthread_mutex_unlock(&p->mu);
                continue;
            }
        }
        /* Detach from the ring now so decode can keep filling while we sleep. */
        int idx = p->vq_r;
        FluxPic pic = p->vq[idx];
        memset(&p->vq[idx], 0, sizeof(p->vq[idx]));
        p->vq_r = (p->vq_r + 1) % 5;
        p->vq_n--;
        pthread_cond_signal(&p->vq_cv);
        pthread_mutex_unlock(&p->mu);
        double pts = pic.pts;
        int gen = pic.gen;

        double fps = p->content_fps;
        if (fps < 20.0 || fps > 120.0 || !isfinite(fps)) fps = 30.0;
        double frame_dur = 1.0 / fps;

        int skip_publish = 0;
        for (;;) {
            pthread_mutex_lock(&p->mu);
            int stop = p->stop;
            int paused = p->paused;
            if (p->resync_clock) {
                p->resync_clock = 0;
                pts0 = NAN;
            }
            pthread_mutex_unlock(&p->mu);
            if (stop) {
                skip_publish = 1;
                break;
            }
            if (paused) {
                usleep(10000);
                if (gen == gen0 && !isnan(pts0)) clock0 += 10000;
                continue;
            }
            /* Absolute film clock (ffplay-style). Never reseat on mild lateness —
             * that dumped the queue at CPU speed (accelerated video). */
            if (gen != gen0 || isnan(pts0) || isnan(pts)) {
                pts0 = isnan(pts) ? 0.0 : pts;
                clock0 = av_gettime_relative();
                gen0 = gen;
            } else {
                double delta = pts - pts0;
                if (delta < 0.5 * frame_dur) delta = frame_dur;
                if (delta > 1.0) delta = 1.0; /* seek / discontinuity cap */
                int64_t target = clock0 + (int64_t)(delta * 1000000.0);
                int64_t now = av_gettime_relative();
                int64_t late = now - target;
                if (late > (int64_t)(frame_dur * 2500000.0)) {
                    /* >2.5 frames late: drop only when a newer picture is
                     * already queued (ffplay). When decode is slower than the
                     * film (CPU 4K), every frame is late — dropping them all
                     * left the screen black while audio kept playing. */
                    pthread_mutex_lock(&p->mu);
                    int newer = p->vq_n > 0;
                    pthread_mutex_unlock(&p->mu);
                    if (newer) skip_publish = 1;
                    pts0 = pts;
                    clock0 = now;
                } else if (target > now + 500) {
                    sleep_until_us(target);
                }
                /* Cap present rate to content fps even when catching up — otherwise
                 * a full queue dumps in <10 ms (accelerated video / black flashes). */
                if (!skip_publish && last_pub_us > 0) {
                    int64_t min_gap = (int64_t)(frame_dur * 0.90 * 1000000.0);
                    int64_t earliest = last_pub_us + min_gap;
                    int64_t n2 = av_gettime_relative();
                    if (earliest > n2 + 500) sleep_until_us(earliest);
                }
            }
            break;
        }

        /* Wait for the soft pump to take the previous publish. The pump no
         * longer holds until GPU upload, so this stays short; overwriting
         * unread pixels mid-refresh caused black flashes. */
        if (!skip_publish) {
            int64_t wait_deadline =
                av_gettime_relative() + (int64_t)(frame_dur * 0.85 * 1000000.0);
            while (av_gettime_relative() < wait_deadline) {
                pthread_mutex_lock(&p->mu);
                int ready = p->frame_ready;
                int stop = p->stop;
                pthread_mutex_unlock(&p->mu);
                if (stop) {
                    skip_publish = 1;
                    break;
                }
                if (!ready) break;
                usleep(500);
            }
        }

        /* Zero-copy: the surface goes to a free GPU slot now, on the film clock. */
        int slot = -1;
        if (!skip_publish && pic.hw) {
            pthread_mutex_lock(&p->mu);
            slot = gpu_pick_slot(p);
            pthread_mutex_unlock(&p->mu);
            if (slot < 0 || !gpu_copy(p, pic.hw, slot)) skip_publish = 1;
        }

        pthread_mutex_lock(&p->mu);
        if (skip_publish) {
            pic_release_locked(p, &pic);
            int stop = p->stop;
            pthread_mutex_unlock(&p->mu);
            if (stop) break;
            continue;
        }
        if (p->frame_rgba) {
            if (!p->spare_rgba) {
                p->spare_rgba = p->frame_rgba;
                p->spare_cap = p->frame_cap;
            } else {
                free(p->frame_rgba);
            }
        }
        p->frame_rgba = pic.buf;
        p->frame_cap = pic.cap;
        p->frame_w = pic.w;
        p->frame_h = pic.h;
        p->frame_pitch = pic.pitch;
        p->frame_format = pic.format;
        p->frame_sar_num = pic.sar_num;
        p->frame_sar_den = pic.sar_den;
        p->frame_matrix = pic.matrix;
        p->frame_full_range = pic.full_range;
        p->frame_kind = pic.hw ? FLUX_FRAME_GPU : FLUX_FRAME_CPU;
        p->frame_slot = slot;
        if (p->last_hw) av_frame_free(&p->last_hw);
        if (pic.hw) {
            p->last_hw = pic.hw;
            p->gpu.last_slot = slot;
        }
        p->frame_ready = 1;
        pthread_cond_signal(&p->vq_cv);
        int stop = p->stop;
        pthread_mutex_unlock(&p->mu);
        last_pub_us = av_gettime_relative();
        if (stop) break;
    }
    return NULL;
}

static int is_hw_pix_fmt(enum AVPixelFormat fmt) {
    switch (fmt) {
    case AV_PIX_FMT_CUDA:
    case AV_PIX_FMT_VAAPI:
    case AV_PIX_FMT_VDPAU:
    case AV_PIX_FMT_DXVA2_VLD:
    case AV_PIX_FMT_D3D11:
    case AV_PIX_FMT_QSV:
    case AV_PIX_FMT_VIDEOTOOLBOX:
        return 1;
    default:
        return 0;
    }
}

/* Prefer negotiated hw pix_fmt; never return NONE (breaks HEVC Main10 / DV). */
static enum AVPixelFormat flux_get_hw_format(AVCodecContext *ctx, const enum AVPixelFormat *pix_fmts) {
    FluxFfmpegPlayer *p = (FluxFfmpegPlayer *)ctx->opaque;
    const enum AVPixelFormat *it;
    if (p && p->hw_pix_fmt != AV_PIX_FMT_NONE) {
        for (it = pix_fmts; *it != AV_PIX_FMT_NONE; it++) {
            if (*it == p->hw_pix_fmt) return *it;
        }
        fprintf(stderr, "flux_ffmpeg: hw pix_fmt %d not offered — software frames\n", (int)p->hw_pix_fmt);
        p->use_hw = 0;
    }
    for (it = pix_fmts; *it != AV_PIX_FMT_NONE; it++) {
        if (!is_hw_pix_fmt(*it)) return *it;
    }
    return pix_fmts[0];
}

/* Prefer largest non-cover video (skip MJPEG attachments). */
static int pick_video_stream(AVFormatContext *fmt) {
    int best = -1;
    int64_t best_area = -1;
    for (unsigned i = 0; i < fmt->nb_streams; i++) {
        AVCodecParameters *par = fmt->streams[i]->codecpar;
        if (!par || par->codec_type != AVMEDIA_TYPE_VIDEO) continue;
        if (par->codec_id == AV_CODEC_ID_MJPEG || par->codec_id == AV_CODEC_ID_PNG) continue;
        int64_t area = (int64_t)par->width * (int64_t)par->height;
        if (area <= 0) area = 1;
        if (area > best_area) {
            best_area = area;
            best = (int)i;
        }
    }
    if (best >= 0) return best;
    return av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
}

static int gpu_name_has(const char *hay, const char *needle) {
    if (!hay || !needle || !needle[0]) return 0;
    size_t n = strlen(needle);
    for (const char *p = hay; *p; p++) {
        size_t i = 0;
        while (i < n && p[i] && tolower((unsigned char)p[i]) == tolower((unsigned char)needle[i])) i++;
        if (i == n) return 1;
    }
    return 0;
}

static int try_init_hw(FluxFfmpegPlayer *p, const AVCodec *codec, AVCodecContext *vctx) {
    /* One GPU does decode and display. FLUXPLAY_GPU is the user's choice
     * (or the panel GPU). Do not open CUDA on a machine whose screen is AMD. */
    const char *gpu = getenv("FLUXPLAY_GPU");
    const char *node = getenv("FLUXPLAY_GPU_NODE");
    int want_nvidia = gpu_name_has(gpu, "nvidia") || gpu_name_has(gpu, "geforce");
    int want_amd = gpu_name_has(gpu, "amd") || gpu_name_has(gpu, "radeon")
        || gpu_name_has(gpu, "rembrandt");
    static const enum AVHWDeviceType kTypes[] = {
#if defined(__ANDROID__)
        AV_HWDEVICE_TYPE_MEDIACODEC,
#endif
        AV_HWDEVICE_TYPE_CUDA,
        AV_HWDEVICE_TYPE_VAAPI,
        AV_HWDEVICE_TYPE_VDPAU,
        AV_HWDEVICE_TYPE_VULKAN,
        AV_HWDEVICE_TYPE_D3D11VA,
        AV_HWDEVICE_TYPE_DXVA2,
        AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
        AV_HWDEVICE_TYPE_QSV,
        AV_HWDEVICE_TYPE_DRM,
        AV_HWDEVICE_TYPE_NONE,
    };
    for (int t = 0; kTypes[t] != AV_HWDEVICE_TYPE_NONE; t++) {
        enum AVHWDeviceType type = kTypes[t];
        enum AVPixelFormat hw_pix = AV_PIX_FMT_NONE;
        for (int i = 0;; i++) {
            const AVCodecHWConfig *cfg = avcodec_get_hw_config(codec, i);
            if (!cfg) break;
            if (!(cfg->methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX)) continue;
            if (cfg->device_type != type) continue;
            hw_pix = cfg->pix_fmt;
            break;
        }
        if (hw_pix == AV_PIX_FMT_NONE) continue;
        if (want_nvidia && type != AV_HWDEVICE_TYPE_CUDA) continue;
        if (want_amd && type != AV_HWDEVICE_TYPE_VAAPI) continue;

        AVBufferRef *dev = NULL;
        AVDictionary *devopts = NULL;
        const char *devname = NULL;
        if (type == AV_HWDEVICE_TYPE_CUDA)
            av_dict_set(&devopts, "primary_ctx", "1", 0);
        if (type == AV_HWDEVICE_TYPE_VAAPI && node && node[0])
            devname = node;
        int derr = av_hwdevice_ctx_create(&dev, type, devname, devopts, 0);
        av_dict_free(&devopts);
        if (derr < 0) continue;

        p->hw_device_ctx = dev;
        p->hw_pix_fmt = hw_pix;
        p->use_hw = 1;
        vctx->hw_device_ctx = av_buffer_ref(dev);
        vctx->opaque = p;
        vctx->get_format = flux_get_hw_format;
        fprintf(stderr, "flux_ffmpeg: hwaccel %s pix_fmt=%d\n", av_hwdevice_get_type_name(type), (int)hw_pix);
        return 1;
    }
    fprintf(stderr, "flux_ffmpeg: no hwaccel (CPU decode)\n");
    return 0;
}

enum { FLUX_AUDIO_RATE = 48000, FLUX_AUDIO_CH = 2, FLUX_AUDIO_Q_CAP = 48000 * 2 * 2 };

static int audio_sink_write(FILE *sink, const void *buf, size_t bytes);

/* PCM queue so a blocking pw-play write never stalls the video decode thread. */
typedef struct AudioQueue {
    pthread_t thread;
    pthread_mutex_t mu;
    pthread_cond_t cv;
    FILE *sink;
    uint8_t data[FLUX_AUDIO_Q_CAP];
    size_t r;
    size_t fill;
    int stop;
    int started;
} AudioQueue;

static int audio_q_push(AudioQueue *q, const void *src, size_t n) {
    if (!q || !q->started || !src || n == 0) return 0;
    pthread_mutex_lock(&q->mu);
    if (q->stop || q->fill + n > FLUX_AUDIO_Q_CAP) {
        pthread_mutex_unlock(&q->mu);
        return 0;
    }
    size_t w = (q->r + q->fill) % FLUX_AUDIO_Q_CAP;
    size_t first = FLUX_AUDIO_Q_CAP - w;
    if (first > n) first = n;
    memcpy(q->data + w, src, first);
    if (n > first) memcpy(q->data, (const uint8_t *)src + first, n - first);
    q->fill += n;
    pthread_cond_signal(&q->cv);
    pthread_mutex_unlock(&q->mu);
    return 1;
}

static void *audio_q_thread(void *arg) {
    AudioQueue *q = (AudioQueue *)arg;
    uint8_t tmp[8192];
    for (;;) {
        pthread_mutex_lock(&q->mu);
        while (q->fill == 0 && !q->stop) pthread_cond_wait(&q->cv, &q->mu);
        if (q->fill == 0 && q->stop) {
            pthread_mutex_unlock(&q->mu);
            break;
        }
        size_t n = q->fill > sizeof(tmp) ? sizeof(tmp) : q->fill;
        size_t first = FLUX_AUDIO_Q_CAP - q->r;
        if (first > n) first = n;
        memcpy(tmp, q->data + q->r, first);
        if (n > first) memcpy(tmp + first, q->data, n - first);
        q->r = (q->r + n) % FLUX_AUDIO_Q_CAP;
        q->fill -= n;
        FILE *sink = q->sink;
        pthread_mutex_unlock(&q->mu);
        if (!sink) break;
        int wr = audio_sink_write(sink, tmp, n);
        if (wr == -2) {
            /* Pipe full: put the bytes back once, then yield. Dropping them
             * was the choppy soundtrack. */
            pthread_mutex_lock(&q->mu);
            if (!q->stop && q->fill + n <= FLUX_AUDIO_Q_CAP) {
                size_t w = (q->r + q->fill) % FLUX_AUDIO_Q_CAP;
                size_t first = FLUX_AUDIO_Q_CAP - w;
                if (first > n) first = n;
                memcpy(q->data + w, tmp, first);
                if (n > first) memcpy(q->data, tmp + first, n - first);
                q->fill += n;
            }
            pthread_mutex_unlock(&q->mu);
            usleep(2000);
            continue;
        }
        if (wr != 0) {
            pthread_mutex_lock(&q->mu);
            q->stop = 1;
            pthread_mutex_unlock(&q->mu);
            break;
        }
    }
    return NULL;
}

static int audio_q_start(AudioQueue *q, FILE *sink) {
    memset(q, 0, sizeof(*q));
    q->sink = sink;
    pthread_mutex_init(&q->mu, NULL);
    pthread_cond_init(&q->cv, NULL);
    int fd = fileno(sink);
    if (fd >= 0) {
        int fl = fcntl(fd, F_GETFL, 0);
        if (fl >= 0) fcntl(fd, F_SETFL, fl | O_NONBLOCK);
    }
    if (pthread_create(&q->thread, NULL, audio_q_thread, q) != 0) return -1;
    q->started = 1;
    return 0;
}

static void *audio_reap(void *arg) {
    pclose((FILE *)arg);
    return NULL;
}

static void audio_q_stop(AudioQueue *q) {
    if (!q || !q->started) return;
    pthread_mutex_lock(&q->mu);
    q->stop = 1;
    pthread_cond_signal(&q->cv);
    pthread_mutex_unlock(&q->mu);
    pthread_join(q->thread, NULL);
    q->started = 0;
    pthread_cond_destroy(&q->cv);
    pthread_mutex_destroy(&q->mu);
}


/** Blocking PCM write — the sink drain rate (sound card clock) IS the master
 * throttle for the whole decode loop: without it, demux/decoding of VOD runs
 * at full CPU speed, audio_clock races ahead, video frames get skip_store'd
 * (frozen/black stage) and excess PCM is dropped (fast-forward sounding audio).
 * EINTR-safe; partial writes resume. */
static int audio_sink_write(FILE *sink, const void *buf, size_t bytes) {
    int fd = fileno(sink);
    if (fd < 0) {
        return fwrite(buf, 1, bytes, sink) == bytes ? 0 : -1;
    }
    const uint8_t *p = (const uint8_t *)buf;
    size_t left = bytes;
    while (left > 0) {
        ssize_t n = write(fd, p, left);
        if (n > 0) {
            p += (size_t)n;
            left -= (size_t)n;
            continue;
        }
        if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            return -2;
        }
        if (n < 0 && errno == EINTR) continue;
        return -1;
    }
    return 0;
}

static FILE *open_audio_sink(int rate, int channels) {
    /* Absolute paths only — never `system("command -v")` / PATH (hijack risk). */
    static const char *pw_play_bins[] = {
        "/usr/bin/pw-play", "/bin/pw-play", "/usr/local/bin/pw-play", NULL};
    static const char *pacat_bins[] = {
        "/usr/bin/pacat", "/bin/pacat", "/usr/local/bin/pacat", NULL};
    static const char *aplay_bins[] = {
        "/usr/bin/aplay", "/bin/aplay", "/usr/local/bin/aplay", NULL};
    char cmd[512];
    FILE *f = NULL;
    for (int i = 0; pw_play_bins[i]; i++) {
        if (access(pw_play_bins[i], X_OK) != 0) continue;
        snprintf(cmd, sizeof(cmd),
                 "exec '%s' -a --format s16 --rate %d --channels %d - 2>/dev/null",
                 pw_play_bins[i], rate, channels);
        f = popen(cmd, "w");
        if (f) {
            fprintf(stderr, "flux_ffmpeg: audio sink %s\n", pw_play_bins[i]);
            setvbuf(f, NULL, _IONBF, 0);
            return f;
        }
    }
    for (int i = 0; pacat_bins[i]; i++) {
        if (access(pacat_bins[i], X_OK) != 0) continue;
        snprintf(cmd, sizeof(cmd),
                 "exec '%s' --raw --format=s16le --rate=%d --channels=%d 2>/dev/null",
                 pacat_bins[i], rate, channels);
        f = popen(cmd, "w");
        if (f) {
            fprintf(stderr, "flux_ffmpeg: audio sink %s\n", pacat_bins[i]);
            setvbuf(f, NULL, _IONBF, 0);
            return f;
        }
    }
    for (int i = 0; aplay_bins[i]; i++) {
        if (access(aplay_bins[i], X_OK) != 0) continue;
        snprintf(cmd, sizeof(cmd), "exec '%s' -q -t raw -f S16_LE -r %d -c %d 2>/dev/null",
                 aplay_bins[i], rate, channels);
        f = popen(cmd, "w");
        if (f) {
            fprintf(stderr, "flux_ffmpeg: audio sink %s\n", aplay_bins[i]);
            setvbuf(f, NULL, _IONBF, 0);
            return f;
        }
    }
    fprintf(stderr, "flux_ffmpeg: no audio sink (install pipewire-utils / pulseaudio-utils)\n");
    return NULL;
}

static void apply_volume_s16(int16_t *samples, int n, float volume) {
    if (volume >= 0.999f) return;
    if (volume <= 0.001f) {
        memset(samples, 0, (size_t)n * sizeof(int16_t));
        return;
    }
    for (int i = 0; i < n; i++) {
        float v = (float)samples[i] * volume;
        if (v > 32767.f) v = 32767.f;
        if (v < -32768.f) v = -32768.f;
        samples[i] = (int16_t)v;
    }
}

static int setup_audio(FluxFfmpegPlayer *p, AVFormatContext *fmt, int *aindex_out,
                       AVCodecContext **actx_out, SwrContext **swr_out, FILE **sink_out) {
    int aindex = av_find_best_stream(fmt, AVMEDIA_TYPE_AUDIO, -1, -1, NULL, 0);
    *aindex_out = -1;
    *actx_out = NULL;
    *swr_out = NULL;
    *sink_out = NULL;
    if (aindex < 0) {
        fprintf(stderr, "flux_ffmpeg: no audio stream\n");
        return 0;
    }
    AVCodecParameters *apar = fmt->streams[aindex]->codecpar;
    const AVCodec *acodec = avcodec_find_decoder(apar->codec_id);
    if (!acodec) {
        fprintf(stderr, "flux_ffmpeg: audio decoder missing\n");
        return 0;
    }
    AVCodecContext *actx = avcodec_alloc_context3(acodec);
    if (!actx) return 0;
    if (avcodec_parameters_to_context(actx, apar) < 0 || avcodec_open2(actx, acodec, NULL) < 0) {
        avcodec_free_context(&actx);
        fprintf(stderr, "flux_ffmpeg: audio open failed\n");
        return 0;
    }

    AVChannelLayout in_ch = {0};
    AVChannelLayout out_ch = {0};
    if (actx->ch_layout.nb_channels > 0) {
        if (av_channel_layout_copy(&in_ch, &actx->ch_layout) < 0) {
            av_channel_layout_default(&in_ch, actx->ch_layout.nb_channels);
        }
    } else {
        av_channel_layout_default(&in_ch, 2);
    }
    av_channel_layout_default(&out_ch, FLUX_AUDIO_CH);

    int in_rate = actx->sample_rate > 0 ? actx->sample_rate : FLUX_AUDIO_RATE;
    enum AVSampleFormat in_fmt =
        actx->sample_fmt != AV_SAMPLE_FMT_NONE ? actx->sample_fmt : AV_SAMPLE_FMT_FLTP;

    SwrContext *swr = NULL;
    if (swr_alloc_set_opts2(&swr, &out_ch, AV_SAMPLE_FMT_S16, FLUX_AUDIO_RATE, &in_ch, in_fmt,
                            in_rate, 0, NULL) < 0 ||
        !swr || swr_init(swr) < 0) {
        if (swr) swr_free(&swr);
        av_channel_layout_uninit(&in_ch);
        av_channel_layout_uninit(&out_ch);
        avcodec_free_context(&actx);
        fprintf(stderr, "flux_ffmpeg: swr_init failed\n");
        return 0;
    }
    av_channel_layout_uninit(&in_ch);
    av_channel_layout_uninit(&out_ch);

    FILE *sink = open_audio_sink(FLUX_AUDIO_RATE, FLUX_AUDIO_CH);
    if (!sink) {
        swr_free(&swr);
        avcodec_free_context(&actx);
        return 0;
    }

    *aindex_out = aindex;
    *actx_out = actx;
    *swr_out = swr;
    *sink_out = sink;
    (void)p;
    fprintf(stderr, "flux_ffmpeg: audio decode ready (stream %d, %d Hz -> %d)\n", aindex, in_rate,
            FLUX_AUDIO_RATE);
    return 1;
}

static void play_audio_frame(FluxFfmpegPlayer *p, AVCodecContext *actx, SwrContext *swr, AudioQueue *aq,
                             AVFrame *frame, AVStream *ast) {
    if (!swr || !aq || !aq->started || !frame) return;
    float volume;
    pthread_mutex_lock(&p->mu);
    volume = p->volume;
    pthread_mutex_unlock(&p->mu);

    if (ast && frame->best_effort_timestamp != AV_NOPTS_VALUE) {
        /* Clock updated only after a successful push — see below. */
    }

    uint8_t *out_planes[1] = {NULL};
    int max_out = swr_get_out_samples(swr, frame->nb_samples);
    if (max_out <= 0) max_out = frame->nb_samples * 4 + 256;
    int out_linesize = 0;
    if (av_samples_alloc(out_planes, &out_linesize, FLUX_AUDIO_CH, max_out, AV_SAMPLE_FMT_S16, 0) <
        0) {
        return;
    }
    int converted = swr_convert(swr, out_planes, max_out, (const uint8_t **)frame->extended_data,
                                frame->nb_samples);
    if (converted > 0) {
        int16_t *pcm = (int16_t *)out_planes[0];
        apply_volume_s16(pcm, converted * FLUX_AUDIO_CH, volume);
        size_t bytes = (size_t)converted * (size_t)FLUX_AUDIO_CH * sizeof(int16_t);
        if (audio_q_push(aq, pcm, bytes)) {
            /* ok */
        }
        /* Advance the master clock even when the PCM ring is full — otherwise
         * video waits forever on a stale aclock and the picture freezes. */
        if (ast && frame->best_effort_timestamp != AV_NOPTS_VALUE) {
            double apts = frame->best_effort_timestamp * av_q2d(ast->time_base);
            pthread_mutex_lock(&p->mu);
            p->audio_clock = apts;
            pthread_mutex_unlock(&p->mu);
        }
    }
    av_freep(&out_planes[0]);
    (void)actx;
}

/* GPU surface → p->xfer. 10-bit (P010) surfaces are downloaded as NV12: the
 * GPU drops the low bits during the copy (output is RGBA8 anyway — identical
 * pixels), halving the bytes read back. On radeonsi a reused P010 destination
 * cost ~37 ms per 4K frame (HEVC Main 10 films fell below 24 fps); NV12 ~10 ms.
 * VAAPI only; drivers that refuse the conversion keep P010. */
static int download_hw_frame(FluxFfmpegPlayer *p, AVFrame *frame) {
    if (!p->xfer) p->xfer = av_frame_alloc();
    AVFrame *sw = p->xfer;
    if (!sw) return -1;
    if (sw->width > 0 && (sw->width != frame->width || sw->height != frame->height))
        av_frame_unref(sw);
    const AVHWFramesContext *fc =
        frame->hw_frames_ctx ? (const AVHWFramesContext *)frame->hw_frames_ctx->data : NULL;
    int ten_bit = fc && fc->sw_format == AV_PIX_FMT_P010;
    /* Only VAAPI converts during the download; CUDA "succeeds" by copying
     * half of every P010 row into the NV12 planes (garbled picture). */
    int try_nv12 = ten_bit && p->xfer_nv12 >= 0 && p->hw_pix_fmt == AV_PIX_FMT_VAAPI;
    if (ten_bit && !try_nv12 && p->hw_pix_fmt == AV_PIX_FMT_VAAPI) {
        /* Fresh destination: reusing it was the slow path (37 vs 15 ms). */
        av_frame_unref(sw);
    }
    if (try_nv12 && !sw->buf[0]) sw->format = AV_PIX_FMT_NV12;
    int r = av_hwframe_transfer_data(sw, frame, 0);
    if (try_nv12 && p->xfer_nv12 == 0) {
        if (r >= 0) {
            p->xfer_nv12 = 1;
            fprintf(stderr, "flux_ffmpeg: 10-bit surfaces downloaded as NV12\n");
        } else {
            p->xfer_nv12 = -1;
            fprintf(stderr, "flux_ffmpeg: NV12 download refused — P010\n");
            av_frame_unref(sw);
            r = av_hwframe_transfer_data(sw, frame, 0);
        }
    }
    return r;
}

/* One decoded video frame → present queue: a CUDA surface reference when the
 * GPU stage has slots (zero-copy), else CPU planes (YUV) or scaled RGBA. */
static void emit_video_frame(FluxFfmpegPlayer *p, AVStream *st, AVFrame *frame, int *frames_out,
                             int *frames_since_flush, int *xfer_err_logged) {
    double pos = 0.0;
    if (frame->best_effort_timestamp != AV_NOPTS_VALUE)
        pos = frame->best_effort_timestamp * av_q2d(st->time_base);
    else if (frame->pts != AV_NOPTS_VALUE)
        pos = frame->pts * av_q2d(st->time_base);
    FluxPic pic;
    memset(&pic, 0, sizeof(pic));
    pic.pts = pos;
    pic.sar_num = frame->sample_aspect_ratio.num > 0 ? frame->sample_aspect_ratio.num : 1;
    pic.sar_den = frame->sample_aspect_ratio.den > 0 ? frame->sample_aspect_ratio.den : 1;
    pic.matrix = frame_matrix(frame);
    pic.full_range = frame->color_range == AVCOL_RANGE_JPEG;

    pthread_mutex_lock(&p->mu);
    p->position = pos;
    int want_w = p->want_w >= 2 ? p->want_w : 1920;
    int want_h = p->want_h >= 2 ? p->want_h : 1080;
    int yuv = p->yuv_out;
    int zero_copy = 0;
    pic.gen = p->play_gen;
    if (yuv && p->use_hw && frame->format == AV_PIX_FMT_CUDA && frame->hw_frames_ctx) {
        const AVHWFramesContext *fc = (const AVHWFramesContext *)frame->hw_frames_ctx->data;
        int bpc = fc->sw_format == AV_PIX_FMT_NV12 ? 1 : fc->sw_format == AV_PIX_FMT_P010 ? 2 : 0;
        if (bpc) {
            FluxGpuSlots *g = &p->gpu;
            g->need_w = frame->width;
            g->need_h = frame->height;
            g->need_bpc = bpc;
            zero_copy = g->attached && g->w == frame->width && g->h == frame->height && g->bpc == bpc;
            if (zero_copy) {
                pic.w = frame->width;
                pic.h = frame->height;
                pic.pitch = g->pitch;
                pic.format = bpc == 2 ? FLUX_FMT_P010 : FLUX_FMT_NV12;
            }
        }
    }
    uint8_t *buf = NULL;
    size_t cap = 0;
    if (!zero_copy) {
        buf = p->spare_rgba;
        cap = p->spare_cap;
        p->spare_rgba = NULL;
        p->spare_cap = 0;
    }
    pthread_mutex_unlock(&p->mu);

    if (zero_copy) {
        pic.hw = av_frame_clone(frame);
        av_frame_unref(frame);
        if (!pic.hw) return;
    } else {
        AVFrame *use = frame;
        if (p->use_hw && frame->format == p->hw_pix_fmt) {
            if (download_hw_frame(p, frame) < 0) {
                if (!*xfer_err_logged) {
                    fprintf(stderr, "flux_ffmpeg: hwframe_transfer failed fmt=%d\n", frame->format);
                    *xfer_err_logged = 1;
                }
                av_frame_unref(frame);
                free(buf);
                return;
            }
            av_frame_unref(frame);
            use = p->xfer;
        }
        int ok;
        if (yuv) {
            ok = store_yuv(&p->sws_yuv, use, &buf, &cap, &pic);
            /* sws brings JPEG-range planes to video range. */
            const char *name = av_get_pix_fmt_name(use->format);
            if (ok && name && strncmp(name, "yuvj", 4) == 0) pic.full_range = 0;
        } else {
            ok = store_frame(&p->sws, use, want_w, want_h, &buf, &cap, &pic.w, &pic.h);
            pic.pitch = pic.w * 4;
            pic.format = FLUX_FMT_RGBA;
            pic.sar_num = pic.sar_den = 1; /* letterboxed with the SAR already */
        }
        if (use == frame) av_frame_unref(frame);
        if (!ok) {
            free(buf);
            return;
        }
        pic.buf = buf;
        pic.cap = cap;
    }

    pthread_mutex_lock(&p->mu);
    vq_push(p, &pic);
    (*frames_out)++;
    (*frames_since_flush)++;
    if (*frames_out == 1)
        fprintf(stderr, "flux_ffmpeg: first frame %s %dx%d%s\n",
                pic.format == FLUX_FMT_RGBA ? "rgba" : pic.format == FLUX_FMT_P010 ? "p010" : "nv12",
                pic.w, pic.h, zero_copy ? " (zero-copy)" : "");
    pthread_mutex_unlock(&p->mu);
}

static void *decode_thread(void *arg) {
    FluxFfmpegPlayer *p = (FluxFfmpegPlayer *)arg;
    AVFormatContext *fmt = NULL;
    AVCodecContext *vctx = NULL;
    AVCodecContext *actx = NULL;
    SwrContext *swr = NULL;
    FILE *audio_sink = NULL;
    AudioQueue audio_q;
    memset(&audio_q, 0, sizeof(audio_q));
    AVPacket *pkt = NULL;
    AVFrame *frame = NULL;
    AVFrame *aframe = NULL;
    int vindex = -1;
    int aindex = -1;

    AVDictionary *opts = NULL;
    if (p->user_agent) {
        char safe_ua[512];
        size_t i, j = 0;
        for (i = 0; p->user_agent[i] && j + 1 < sizeof(safe_ua); i++) {
            unsigned char c = (unsigned char)p->user_agent[i];
            if (c == '\r' || c == '\n') continue;
            safe_ua[j++] = (char)c;
        }
        safe_ua[j] = 0;
        av_dict_set(&opts, "user_agent", safe_ua[0] ? safe_ua : "IPTVSmartersPlayer", 0);
    } else {
        av_dict_set(&opts, "user_agent", "IPTVSmartersPlayer", 0);
    }
    if (p->referer) {
        char hdr[1024];
        char safe[768];
        size_t i, j = 0;
        /* Strip CR/LF so a malicious Referer cannot inject lavf HTTP headers. */
        for (i = 0; p->referer[i] && j + 1 < sizeof(safe); i++) {
            unsigned char c = (unsigned char)p->referer[i];
            if (c == '\r' || c == '\n') continue;
            safe[j++] = (char)c;
        }
        safe[j] = 0;
        if (safe[0]) {
            snprintf(hdr, sizeof(hdr), "Referer: %s\r\n", safe);
            av_dict_set(&opts, "headers", hdr, 0);
        }
    }
    if (p->http_proxy && p->http_proxy[0]) {
        char safe_px[768];
        size_t i, j = 0;
        for (i = 0; p->http_proxy[i] && j + 1 < sizeof(safe_px); i++) {
            unsigned char c = (unsigned char)p->http_proxy[i];
            if (c == '\r' || c == '\n') continue;
            safe_px[j++] = (char)c;
        }
        safe_px[j] = 0;
        if (safe_px[0]) av_dict_set(&opts, "http_proxy", safe_px, 0);
    }
    /* file: only for a movie this app downloaded (path under FLUXPLAY_DOWNLOAD_ROOT).
     * A remote playlist must not be able to open arbitrary local paths. */
    {
        const char *root = getenv("FLUXPLAY_DOWNLOAD_ROOT");
        int local = 0;
        if (p->url && root && root[0] && strncmp(p->url, "file:", 5) == 0
            && !strstr(p->url, "..")) {
            /* The path must start with the root, on a directory boundary. */
            const char *path = p->url + 5;
            if (strncmp(path, "//", 2) == 0) path += 2;
            size_t rl = strlen(root);
            while (rl > 1 && root[rl - 1] == '/') rl--;
            local = strncmp(path, root, rl) == 0 && (path[rl] == '/' || path[rl] == '\0');
        }
        /* http_proxy only covers http(s); rtmp/rtsp/udp/srt would bypass the tunnel. */
        int proxied = p->http_proxy && p->http_proxy[0];
        av_dict_set(&opts, "protocol_whitelist",
                    local ? "file,crypto,http,https,tcp,tls,rtmp,rtmps,rtsp,rtsps,rtp,udp,srt"
                    : proxied ? "crypto,http,https,httpproxy,tcp,tls"
                              : "crypto,http,https,tcp,tls,rtmp,rtmps,rtsp,rtsps,rtp,udp,srt",
                    0);
        if (!local) {
            av_dict_set(&opts, "reconnect", "1", 0);
            av_dict_set(&opts, "reconnect_streamed", "1", 0);
        }
    }
    av_dict_set(&opts, "reconnect_delay_max", "5", 0);
    if (p->low_latency) {
        av_dict_set(&opts, "fflags", "nobuffer", 0);
        av_dict_set(&opts, "flags", "low_delay", 0);
    } else {
        /* Remote VOD (HEVC 4K) needs a larger probe window than live low-latency. */
        av_dict_set(&opts, "probesize", "32000000", 0);
        av_dict_set(&opts, "analyzeduration", "15000000", 0);
    }

    fmt = avformat_alloc_context();
    if (!fmt) goto done;
    fmt->interrupt_callback.callback = interrupted;
    fmt->interrupt_callback.opaque = p;

    if (avformat_open_input(&fmt, p->url, NULL, &opts) < 0) {
        fprintf(stderr, "flux_ffmpeg: open_input failed\n");
        goto done;
    }
    av_dict_free(&opts);
    opts = NULL;

    if (avformat_find_stream_info(fmt, NULL) < 0) {
        fprintf(stderr, "flux_ffmpeg: find_stream_info failed\n");
        goto done;
    }

    vindex = pick_video_stream(fmt);
    if (vindex < 0) {
        fprintf(stderr, "flux_ffmpeg: no video stream\n");
        goto done;
    }

    {
        AVStream *st = fmt->streams[vindex];
        const AVCodec *codec = avcodec_find_decoder(st->codecpar->codec_id);
        if (!codec) goto done;
        fprintf(stderr,
                "flux_ffmpeg: stream=%d codec=%s %dx%d\n",
                vindex,
                codec->name ? codec->name : "?",
                st->codecpar->width,
                st->codecpar->height);
        {
            AVRational rate = st->avg_frame_rate.num > 0 ? st->avg_frame_rate : st->r_frame_rate;
            double fps = 0.0;
            if (rate.num > 0 && rate.den > 0)
                fps = av_q2d(rate);
            if (fps >= 20.0 && fps <= 120.0) {
                p->content_fps = fps;
                fprintf(stderr, "flux_ffmpeg: content_fps=%.3f\n", fps);
            } else {
                p->content_fps = 30.0;
                fprintf(stderr, "flux_ffmpeg: content_fps unknown — default 30\n");
            }
        }
        vctx = avcodec_alloc_context3(codec);
        if (!vctx) goto done;
        if (avcodec_parameters_to_context(vctx, st->codecpar) < 0) goto done;
        vctx->pkt_timebase = st->time_base;
        vctx->thread_count = 0;
        if (p->want_hwdec) {
            try_init_hw(p, codec, vctx);
            /* One decode thread + a modest surface pool. Auto threads asked for
             * 40 surfaces and CUDA rejected the decoder (opening hitch). */
            if (p->use_hw) {
                vctx->thread_count = 1;
                vctx->extra_hw_frames = 8;
            }
        } else {
            fprintf(stderr, "flux_ffmpeg: hwdec disabled by opts\n");
        }
        if (avcodec_open2(vctx, codec, NULL) < 0) {
            /* Retry without hw if open failed */
            if (p->use_hw) {
                fprintf(stderr, "flux_ffmpeg: hw open failed — CPU fallback\n");
                if (vctx->hw_device_ctx) av_buffer_unref(&vctx->hw_device_ctx);
                if (p->hw_device_ctx) av_buffer_unref(&p->hw_device_ctx);
                p->use_hw = 0;
                p->hw_pix_fmt = AV_PIX_FMT_NONE;
                vctx->get_format = NULL;
                vctx->opaque = NULL;
                if (avcodec_open2(vctx, codec, NULL) < 0) goto done;
            } else {
                goto done;
            }
        }

        if (fmt->duration > 0) {
            pthread_mutex_lock(&p->mu);
            p->duration = (double)fmt->duration / (double)AV_TIME_BASE;
            pthread_mutex_unlock(&p->mu);
        }
    }

    setup_audio(p, fmt, &aindex, &actx, &swr, &audio_sink);
    if (audio_sink && audio_q_start(&audio_q, audio_sink) != 0) {
        fprintf(stderr, "flux_ffmpeg: audio thread failed\n");
        pclose(audio_sink);
        audio_sink = NULL;
    }

    pkt = av_packet_alloc();
    frame = av_frame_alloc();
    aframe = av_frame_alloc();
    if (!pkt || !frame || !aframe) goto done;

    /* Other audio languages / subtitles would be parsed and dropped anyway. */
    for (unsigned i = 0; i < fmt->nb_streams; i++) {
        if ((int)i != vindex && (int)i != aindex) fmt->streams[i]->discard = AVDISCARD_ALL;
    }
    {
        /* VOD: up to a minute ahead (ExoPlayer maxBuffer 50 s, mpv readahead).
         * Live cannot run ahead of the broadcast; keep latency bounded. */
#ifdef __ANDROID__
        size_t max_mb = 96;
#else
        size_t max_mb = 256;
#endif
        const char *env = getenv("FLUXPLAY_READAHEAD_MB");
        if (env && atoi(env) >= 8 && atoi(env) <= 4096) max_mb = (size_t)atoi(env);
        pthread_mutex_lock(&p->mu);
        p->fmt = fmt;
        p->vindex = vindex;
        p->aindex = aindex;
        p->pq_max_bytes = (p->low_latency ? 32 : max_mb) * 1024 * 1024;
        p->pq_max_secs = p->low_latency ? 10.0 : 60.0;
        pthread_mutex_unlock(&p->mu);
        if (pthread_create(&p->demux_thread, NULL, demux_thread, p) != 0) {
            fprintf(stderr, "flux_ffmpeg: demux thread failed\n");
            goto done;
        }
        p->demux_started = 1;
    }

    fprintf(stderr, "flux_ffmpeg: decode loop start\n");

    int64_t clock0 = av_gettime_relative();
    double pts0 = NAN;
    int pause_flushed = 0;
    int send_err_logged = 0;
    int xfer_err_logged = 0;
    int frames_out = 0;
    int frames_since_flush = 0;
    int skipped_run = 0;
    int cur_serial = 0;
    double rebuffer_goal = 0.0;
    int64_t last_rebuffer_us = 0;

    while (1) {
        int stop, paused, serial;
        double seek_secs;
        pthread_mutex_lock(&p->mu);
        stop = p->stop;
        paused = p->paused;
        serial = p->pq_serial;
        seek_secs = p->pq_flush_secs;
        pthread_mutex_unlock(&p->mu);
        if (stop) break;

        if (serial != cur_serial) {
            /* The demux thread seeked and flushed the packet queue. */
            cur_serial = serial;
            frames_since_flush = 0;
            {
                avcodec_flush_buffers(vctx);
                if (actx) avcodec_flush_buffers(actx);
                if (swr) swr_convert(swr, NULL, 0, NULL, 0);
                pthread_mutex_lock(&p->mu);
                p->position = seek_secs;
                p->audio_clock = NAN;
                p->frame_ready = 0;
                p->play_gen++;
                p->vq_n = 0;
                while (p->job_n > 0) {
                    AVFrame *f = p->jobs[p->job_r].frame;
                    p->jobs[p->job_r].frame = NULL;
                    p->job_r = (p->job_r + 1) % 8;
                    p->job_n--;
                    if (f) av_frame_free(&f);
                }
                pthread_cond_broadcast(&p->vq_cv);
                pthread_cond_broadcast(&p->job_cv);
                pthread_mutex_unlock(&p->mu);
                pts0 = NAN;
                clock0 = av_gettime_relative();
                if (audio_q.started) {
                    int16_t z[FLUX_AUDIO_RATE / 10 * FLUX_AUDIO_CH];
                    memset(z, 0, sizeof(z));
                    audio_q_push(&audio_q, z, sizeof(z));
                }
            }
        }

        if (paused) {
            /* Drain residual PCM in pw-play/pacat buffer so pause is silent. */
            if (audio_q.started && !pause_flushed) {
                int16_t z[FLUX_AUDIO_RATE / 10 * FLUX_AUDIO_CH];
                memset(z, 0, sizeof(z));
                audio_q_push(&audio_q, z, sizeof(z));
                pause_flushed = 1;
            }
            usleep(20000);
            clock0 = av_gettime_relative();
            pts0 = NAN;
            continue;
        }
        pause_flushed = 0;

        {
            PktNode *n = NULL;
            int eof = 0;
            pthread_mutex_lock(&p->mu);
            if (!p->pq_head && !p->pq_eof && !p->low_latency && frames_since_flush > 0 &&
                p->pq_serial == cur_serial && !p->stop) {
                /* Ran dry mid-film: hold until a real cushion is back instead of
                 * playing a few frames per network burst (mpv cache-pause with
                 * separate fill threshold, ExoPlayer bufferForPlaybackAfterRebuffer).
                 * Repeated stalls → longer, rarer pauses. */
                int64_t now = av_gettime_relative();
                double fps_now = p->content_fps >= 20.0 ? p->content_fps : 30.0;
                /* Dry right after open/seek is the initial fill (ExoPlayer
                 * bufferForPlayback), not a sign the link is too slow. */
                int initial = frames_since_flush < (int)(2.0 * fps_now);
                double goal;
                if (initial) {
                    goal = 2.0;
                } else {
                    if (last_rebuffer_us > 0 && now - last_rebuffer_us < 120 * 1000000LL)
                        rebuffer_goal = fmin(rebuffer_goal * 2.0, 30.0);
                    else
                        rebuffer_goal = 3.0;
                    goal = rebuffer_goal;
                }
                goal = fmin(goal, p->pq_max_secs * 0.8);
                size_t goal_bytes = p->pq_max_bytes / 10 * 9;
                p->buffer_goal = goal;
                p->buffering = 1;
                fprintf(stderr, "flux_ffmpeg: rebuffering (goal %.0fs, net %.1f Mbit/s, media %.1f Mbit/s)\n",
                        goal, p->net_bps / 1e6, p->media_bps / 1e6);
                while (!p->stop && !p->pq_eof && p->pq_serial == cur_serial && p->pq_secs < goal &&
                       p->pq_bytes < goal_bytes) {
                    timed_wait_ms(&p->pq_cv, &p->mu, 50);
                }
                p->buffering = 0;
                p->resync_clock = 1;
                int64_t end = av_gettime_relative();
                if (!initial) last_rebuffer_us = end;
                fprintf(stderr, "flux_ffmpeg: rebuffered %.1fs in %.1fs\n", p->pq_secs,
                        (double)(end - now) / 1e6);
            }
            while (!p->pq_head && !p->pq_eof && !p->stop && p->pq_serial == cur_serial) {
                timed_wait_ms(&p->pq_cv, &p->mu, 50);
            }
            if (p->pq_serial == cur_serial) n = pq_pop(p);
            eof = !n && p->pq_eof && p->pq_serial == cur_serial;
            pthread_mutex_unlock(&p->mu);
            if (!n) {
                if (eof) break;
                continue;
            }
            av_packet_move_ref(pkt, n->pkt);
            av_packet_free(&n->pkt);
            free(n);
        }

        if (pkt->stream_index == aindex && actx && swr && audio_q.started) {
            int sret = avcodec_send_packet(actx, pkt);
            av_packet_unref(pkt);
            if (sret < 0 && sret != AVERROR(EAGAIN) && sret != AVERROR_EOF) {
                continue;
            }
            while (1) {
                int rret = avcodec_receive_frame(actx, aframe);
                if (rret == AVERROR(EAGAIN) || rret == AVERROR_EOF) break;
                if (rret < 0) break;
                play_audio_frame(p, actx, swr, &audio_q, aframe, fmt->streams[aindex]);
                av_frame_unref(aframe);
            }
            continue;
        }

        if (pkt->stream_index != vindex) {
            av_packet_unref(pkt);
            continue;
        }

        {
            /* Send may return EAGAIN when the decoder still holds pictures — drain, then resend. */
            for (;;) {
                int sret = avcodec_send_packet(vctx, pkt);
                if (sret != AVERROR(EAGAIN)) {
                    av_packet_unref(pkt);
                    if (sret < 0 && sret != AVERROR_EOF) {
                        if (!send_err_logged) {
                            char errbuf[128];
                            av_strerror(sret, errbuf, sizeof(errbuf));
                            fprintf(stderr, "flux_ffmpeg: send_packet: %s\n", errbuf);
                            send_err_logged = 1;
                        }
                    } else {
                        while (1) {
                            int rret = avcodec_receive_frame(vctx, frame);
                            if (rret == AVERROR(EAGAIN) || rret == AVERROR_EOF) break;
                            if (rret < 0) {
                                if (!send_err_logged) {
                                    char errbuf[128];
                                    av_strerror(rret, errbuf, sizeof(errbuf));
                                    fprintf(stderr, "flux_ffmpeg: receive_frame: %s\n", errbuf);
                                    send_err_logged = 1;
                                }
                                break;
                            }
                            emit_video_frame(p, fmt->streams[vindex], frame, &frames_out, &frames_since_flush, &xfer_err_logged);
                        }
                    }
                    break;
                }
                /* EAGAIN: drain one frame then retry the same packet. */
                {
                    int rret = avcodec_receive_frame(vctx, frame);
                    if (rret < 0) {
                        av_packet_unref(pkt);
                        break;
                    }
                    emit_video_frame(p, fmt->streams[vindex], frame, &frames_out, &frames_since_flush, &xfer_err_logged);
                }
            }
        }
    }

done:
    pthread_mutex_lock(&p->mu);
    p->pq_abort = 1;
    p->buffering = 0;
    pthread_cond_broadcast(&p->pq_cv);
    pthread_mutex_unlock(&p->mu);
    if (p->demux_started) {
        pthread_join(p->demux_thread, NULL);
        p->demux_started = 0;
    }
    pthread_mutex_lock(&p->mu);
    pq_clear(p);
    p->fmt = NULL;
    if (p->net_bps > 0.0)
        fprintf(stderr, "flux_ffmpeg: read-ahead net %.1f Mbit/s, media %.1f Mbit/s\n",
                p->net_bps / 1e6, p->media_bps / 1e6);
    pthread_mutex_unlock(&p->mu);
    av_dict_free(&opts);
    if (aframe) av_frame_free(&aframe);
    if (frame) av_frame_free(&frame);
    if (pkt) av_packet_free(&pkt);
    if (swr) swr_free(&swr);
    if (actx) avcodec_free_context(&actx);
    audio_q_stop(&audio_q);
    if (audio_sink) {
        /* pw-play can drain for seconds. Don't stall the decode thread (and UI stop). */
        FILE *sink = audio_sink;
        pthread_t reaper;
        if (pthread_create(&reaper, NULL, audio_reap, sink) == 0) pthread_detach(reaper);
        else pclose(sink);
    }
    if (vctx) avcodec_free_context(&vctx);
    if (fmt) avformat_close_input(&fmt);

    pthread_mutex_lock(&p->mu);
    p->alive = 0;
    pthread_mutex_unlock(&p->mu);
    fprintf(stderr, "flux_ffmpeg: decode loop exit frames=%d\n", frames_out);
    return NULL;
}
