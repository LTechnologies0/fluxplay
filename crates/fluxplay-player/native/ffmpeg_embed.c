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

#include <math.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

void flux_ffmpeg_set_av_log_level(int level) {
    av_log_set_level(level);
    fprintf(stderr, "flux_ffmpeg: av_log_set_level(%d)\n", level);
}

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
    int frame_w;
    int frame_h;
    int frame_ready;
    int want_w;
    int want_h;
    int eof_retries;
    struct SwsContext *sws;
    AVBufferRef *hw_device_ctx;
    enum AVPixelFormat hw_pix_fmt;
    int use_hw;

    char *url;
    char *user_agent;
    char *referer;
    char *http_proxy;
    int low_latency;
};

static void *decode_thread(void *arg);

static char *flux_strdup(const char *s) {
    if (!s) return NULL;
    size_t n = strlen(s) + 1;
    char *d = (char *)malloc(n);
    if (d) memcpy(d, s, n);
    return d;
}

FluxFfmpegPlayer *flux_ffmpeg_open(const FluxFfmpegOpenOpts *opts) {
    if (!opts || !opts->url || !opts->url[0]) return NULL;

    avformat_network_init();

    FluxFfmpegPlayer *p = (FluxFfmpegPlayer *)calloc(1, sizeof(*p));
    if (!p) return NULL;
    pthread_mutex_init(&p->mu, NULL);
    p->url = flux_strdup(opts->url);
    p->user_agent = flux_strdup(opts->user_agent);
    p->referer = flux_strdup(opts->referer);
    p->http_proxy = flux_strdup(opts->http_proxy);
    p->low_latency = opts->low_latency;
    p->want_hwdec = opts->hwdec ? 1 : 0;
    p->volume = 1.0f;
    p->alive = 1;
    p->audio_clock = NAN;
    p->want_w = 1920;
    p->want_h = 1080;
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
    pthread_mutex_unlock(&p->mu);
    if (p->thread) {
        pthread_join(p->thread, NULL);
        p->thread = 0;
    }
    free(p->frame_rgba);
    if (p->sws) sws_freeContext(p->sws);
    if (p->hw_device_ctx) av_buffer_unref(&p->hw_device_ctx);
    free(p->url);
    free(p->user_agent);
    free(p->referer);
    free(p->http_proxy);
    pthread_mutex_destroy(&p->mu);
    free(p);
}

int flux_ffmpeg_pull_rgba(FluxFfmpegPlayer *p, uint8_t *out, int out_w, int out_h) {
    if (!p || !out || out_w < 2 || out_h < 2) return 0;
    pthread_mutex_lock(&p->mu);
    p->want_w = out_w > 3840 ? 3840 : out_w;
    p->want_h = out_h > 2160 ? 2160 : out_h;
    if (!p->frame_ready || !p->frame_rgba) {
        pthread_mutex_unlock(&p->mu);
        return 0;
    }
    /* Size mismatch: retarget decode want_* only. Keep the unread frame so the
     * iced stage can hold the last GPU texture; has_frame gates on size match
     * so we don't spin-alloc until store emits the new dims. */
    if (p->frame_w != out_w || p->frame_h != out_h) {
        p->want_w = out_w > 3840 ? 3840 : out_w;
        p->want_h = out_h > 2160 ? 2160 : out_h;
        pthread_mutex_unlock(&p->mu);
        return 0;
    }
    memcpy(out, p->frame_rgba, (size_t)out_w * (size_t)out_h * 4);
    /* Consume frame so UI doesn't re-upload the same pixels every 8ms tick. */
    p->frame_ready = 0;
    pthread_mutex_unlock(&p->mu);
    return 1;
}

void flux_ffmpeg_set_output_size(FluxFfmpegPlayer *p, int w, int h) {
    if (!p || w < 2 || h < 2) return;
    pthread_mutex_lock(&p->mu);
    p->want_w = w > 3840 ? 3840 : w;
    p->want_h = h > 2160 ? 2160 : h;
    pthread_mutex_unlock(&p->mu);
}

int flux_ffmpeg_is_alive(FluxFfmpegPlayer *p) {
    if (!p) return 0;
    pthread_mutex_lock(&p->mu);
    int a = p->alive && !p->stop;
    pthread_mutex_unlock(&p->mu);
    return a;
}

int flux_ffmpeg_has_frame(FluxFfmpegPlayer *p) {
    if (!p) return 0;
    pthread_mutex_lock(&p->mu);
    /* Only report ready when dims match want — avoids pull→mismatch spin.
     * Decode skip_store also keys on size match so retargets still store. */
    int r = p->frame_ready
        && p->frame_rgba
        && p->frame_w == p->want_w
        && p->frame_h == p->want_h;
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

static int interrupted(void *opaque) {
    FluxFfmpegPlayer *p = (FluxFfmpegPlayer *)opaque;
    int s;
    pthread_mutex_lock(&p->mu);
    s = p->stop;
    pthread_mutex_unlock(&p->mu);
    return s;
}

static void store_frame(FluxFfmpegPlayer *p, AVFrame *frame, int target_w, int target_h) {
    if (!frame || frame->width < 2 || frame->height < 2) return;
    int tw = target_w > 0 ? target_w : frame->width;
    int th = target_h > 0 ? target_h : frame->height;
    if (tw > 3840) tw = 3840;
    if (th > 2160) th = 2160;

    struct SwsContext *sws = sws_getCachedContext(
        p->sws,
        frame->width,
        frame->height,
        (enum AVPixelFormat)frame->format,
        tw,
        th,
        AV_PIX_FMT_RGBA,
        SWS_FAST_BILINEAR,
        NULL,
        NULL,
        NULL);
    if (!sws) return;
    p->sws = sws;

    size_t need = (size_t)tw * (size_t)th * 4;
    pthread_mutex_lock(&p->mu);
    uint8_t *dst = p->frame_rgba;
    if (!dst || (size_t)p->frame_w * (size_t)p->frame_h * 4 != need) {
        free(dst);
        dst = (uint8_t *)malloc(need);
        if (!dst) {
            pthread_mutex_unlock(&p->mu);
            return;
        }
        p->frame_rgba = dst;
    }
    /* Scale into the retained buffer (no per-frame malloc). */
    uint8_t *dst_slices[4] = {dst, NULL, NULL, NULL};
    int dst_stride[4] = {tw * 4, 0, 0, 0};
    /* Unlock during sws_scale — pull_rgba only reads when frame_ready. */
    p->frame_ready = 0;
    pthread_mutex_unlock(&p->mu);

    sws_scale(sws, (const uint8_t *const *)frame->data, frame->linesize, 0, frame->height, dst_slices, dst_stride);

    pthread_mutex_lock(&p->mu);
    /* Recheck pointer still ours (destroy/stop could race — stop joins thread first). */
    if (p->frame_rgba == dst) {
        p->frame_w = tw;
        p->frame_h = th;
        p->frame_ready = 1;
    }
    pthread_mutex_unlock(&p->mu);
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

static int try_init_hw(FluxFfmpegPlayer *p, const AVCodec *codec, AVCodecContext *vctx) {
    static const enum AVHWDeviceType kTypes[] = {
        AV_HWDEVICE_TYPE_CUDA,
        AV_HWDEVICE_TYPE_VAAPI,
        AV_HWDEVICE_TYPE_VDPAU,
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

        AVBufferRef *dev = NULL;
        if (av_hwdevice_ctx_create(&dev, type, NULL, NULL, 0) < 0) continue;

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

enum { FLUX_AUDIO_RATE = 48000, FLUX_AUDIO_CH = 2 };

static FILE *open_audio_sink(int rate, int channels) {
    /* Resolve via PATH (Nix/Homebrew/custom prefixes), not hardcoded /usr/bin. */
    char cmd[384];
    FILE *f = NULL;
    if (system("command -v pw-play >/dev/null 2>&1") == 0) {
        snprintf(cmd, sizeof(cmd),
                 "exec pw-play -a --format s16 --rate %d --channels %d - 2>/dev/null", rate,
                 channels);
        f = popen(cmd, "w");
        if (f) {
            fprintf(stderr, "flux_ffmpeg: audio sink pw-play %d Hz / %d ch\n", rate, channels);
            setvbuf(f, NULL, _IONBF, 0);
            return f;
        }
    }
    if (system("command -v pacat >/dev/null 2>&1") == 0) {
        snprintf(cmd, sizeof(cmd),
                 "exec pacat --raw --format=s16le --rate=%d --channels=%d 2>/dev/null", rate,
                 channels);
        f = popen(cmd, "w");
        if (f) {
            fprintf(stderr, "flux_ffmpeg: audio sink pacat %d Hz / %d ch\n", rate, channels);
            setvbuf(f, NULL, _IONBF, 0);
            return f;
        }
    }
    if (system("command -v aplay >/dev/null 2>&1") == 0) {
        snprintf(cmd, sizeof(cmd), "exec aplay -q -t raw -f S16_LE -r %d -c %d 2>/dev/null", rate,
                 channels);
        f = popen(cmd, "w");
        if (f) {
            fprintf(stderr, "flux_ffmpeg: audio sink aplay %d Hz / %d ch\n", rate, channels);
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

static void play_audio_frame(FluxFfmpegPlayer *p, AVCodecContext *actx, SwrContext *swr, FILE *sink,
                             AVFrame *frame, AVStream *ast) {
    if (!swr || !sink || !frame) return;
    float volume;
    pthread_mutex_lock(&p->mu);
    volume = p->volume;
    pthread_mutex_unlock(&p->mu);

    if (ast && frame->best_effort_timestamp != AV_NOPTS_VALUE) {
        double apts = frame->best_effort_timestamp * av_q2d(ast->time_base);
        pthread_mutex_lock(&p->mu);
        p->audio_clock = apts;
        pthread_mutex_unlock(&p->mu);
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
        if (fwrite(pcm, 1, bytes, sink) != bytes) {
            /* sink died — ignore further writes this session */
            fprintf(stderr, "flux_ffmpeg: audio sink write failed\n");
        }
    }
    av_freep(&out_planes[0]);
    (void)actx;
}

static void *decode_thread(void *arg) {
    FluxFfmpegPlayer *p = (FluxFfmpegPlayer *)arg;
    AVFormatContext *fmt = NULL;
    AVCodecContext *vctx = NULL;
    AVCodecContext *actx = NULL;
    SwrContext *swr = NULL;
    FILE *audio_sink = NULL;
    AVPacket *pkt = NULL;
    AVFrame *frame = NULL;
    AVFrame *aframe = NULL;
    int vindex = -1;
    int aindex = -1;
    int want_w = 1920;
    int want_h = 1080;

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
    /* Block nested playlist URLs from opening file:/concat:/crypto: etc. */
    av_dict_set(&opts, "protocol_whitelist",
                "file,http,https,tcp,tls,rtmp,rtmps,rtsp,rtsps,rtp,udp,srt,crypto,data", 0);
    av_dict_set(&opts, "reconnect", "1", 0);
    av_dict_set(&opts, "reconnect_streamed", "1", 0);
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
        vctx = avcodec_alloc_context3(codec);
        if (!vctx) goto done;
        if (avcodec_parameters_to_context(vctx, st->codecpar) < 0) goto done;
        vctx->pkt_timebase = st->time_base;
        vctx->thread_count = 0; /* auto */
        if (p->want_hwdec) {
            try_init_hw(p, codec, vctx);
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

    pkt = av_packet_alloc();
    frame = av_frame_alloc();
    aframe = av_frame_alloc();
    if (!pkt || !frame || !aframe) goto done;

    fprintf(stderr, "flux_ffmpeg: decode loop start\n");

    int64_t clock0 = av_gettime_relative();
    double pts0 = NAN;
    int pause_flushed = 0;
    int send_err_logged = 0;
    int xfer_err_logged = 0;
    int frames_out = 0;

    while (1) {
        int stop, paused, seek_req;
        double seek_secs;
        pthread_mutex_lock(&p->mu);
        stop = p->stop;
        paused = p->paused;
        seek_req = p->seek_req;
        seek_secs = p->seek_secs;
        want_w = p->want_w >= 2 ? p->want_w : 1920;
        want_h = p->want_h >= 2 ? p->want_h : 1080;
        pthread_mutex_unlock(&p->mu);
        if (stop) break;

        if (seek_req) {
            int64_t ts = (int64_t)(seek_secs * AV_TIME_BASE);
            int sret = av_seek_frame(fmt, -1, ts, AVSEEK_FLAG_BACKWARD);
            if (sret < 0) {
                pthread_mutex_lock(&p->mu);
                p->seek_req = 0;
                pthread_mutex_unlock(&p->mu);
            } else {
                avcodec_flush_buffers(vctx);
                if (actx) avcodec_flush_buffers(actx);
                if (swr) swr_convert(swr, NULL, 0, NULL, 0);
                pthread_mutex_lock(&p->mu);
                p->seek_req = 0;
                p->position = seek_secs;
                p->audio_clock = NAN;
                p->frame_ready = 0;
                pthread_mutex_unlock(&p->mu);
                pts0 = NAN;
                clock0 = av_gettime_relative();
                if (audio_sink) {
                    int16_t z[FLUX_AUDIO_RATE / 5 * FLUX_AUDIO_CH]; /* ~200ms */
                    memset(z, 0, sizeof(z));
                    (void)fwrite(z, 1, sizeof(z), audio_sink);
                }
            }
        }

        if (paused) {
            /* Drain residual PCM in pw-play/pacat buffer so pause is silent. */
            if (audio_sink && !pause_flushed) {
                int16_t z[FLUX_AUDIO_RATE / 5 * FLUX_AUDIO_CH]; /* ~200ms */
                memset(z, 0, sizeof(z));
                (void)fwrite(z, 1, sizeof(z), audio_sink);
                pause_flushed = 1;
            }
            usleep(20000);
            clock0 = av_gettime_relative();
            pts0 = NAN;
            continue;
        }
        pause_flushed = 0;

        int r = av_read_frame(fmt, pkt);
        if (r < 0) {
            /* Live/HLS: lavf often returns EOF between playlist reloads — don't die. */
            if (r == AVERROR_EOF) {
                /* Live/HLS: duration often unknown — retry briefly.
                 * Hard-cap retries (no mid-stream reset) so duration≤0 VOD cannot spin forever. */
                int live = fmt->duration <= 0;
                pthread_mutex_lock(&p->mu);
                int stop = p->stop;
                if (live && !stop && p->eof_retries++ < 120) { /* ~30s at 250ms */
                    pthread_mutex_unlock(&p->mu);
                    usleep(250000);
                    continue;
                }
                pthread_mutex_unlock(&p->mu);
                break;
            }
            usleep(10000);
            continue;
        }

        if (pkt->stream_index == aindex && actx && swr && audio_sink) {
            int sret = avcodec_send_packet(actx, pkt);
            av_packet_unref(pkt);
            if (sret < 0 && sret != AVERROR(EAGAIN) && sret != AVERROR_EOF) {
                continue;
            }
            while (1) {
                int rret = avcodec_receive_frame(actx, aframe);
                if (rret == AVERROR(EAGAIN) || rret == AVERROR_EOF) break;
                if (rret < 0) break;
                play_audio_frame(p, actx, swr, audio_sink, aframe, fmt->streams[aindex]);
                av_frame_unref(aframe);
            }
            continue;
        }

        if (pkt->stream_index != vindex) {
            av_packet_unref(pkt);
            continue;
        }

        {
            int sret = avcodec_send_packet(vctx, pkt);
            av_packet_unref(pkt);
            if (sret < 0 && sret != AVERROR(EAGAIN) && sret != AVERROR_EOF) {
                if (!send_err_logged) {
                    char errbuf[128];
                    av_strerror(sret, errbuf, sizeof(errbuf));
                    fprintf(stderr, "flux_ffmpeg: send_packet: %s\n", errbuf);
                    send_err_logged = 1;
                }
                continue;
            }

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

                AVStream *st = fmt->streams[vindex];
                double pos = 0.0;
                if (frame->best_effort_timestamp != AV_NOPTS_VALUE) {
                    pos = frame->best_effort_timestamp * av_q2d(st->time_base);
                } else if (frame->pts != AV_NOPTS_VALUE) {
                    pos = frame->pts * av_q2d(st->time_base);
                }
                pthread_mutex_lock(&p->mu);
                p->position = pos;
                pthread_mutex_unlock(&p->mu);

                AVFrame *use = frame;
                AVFrame *sw = NULL;
                int skip_store = 0;
                pthread_mutex_lock(&p->mu);
                want_w = p->want_w >= 2 ? p->want_w : 1920;
                want_h = p->want_h >= 2 ? p->want_h : 1080;
                /* Overwrite unread RGBA so GPU-busy UI still gets latest on next pull.
                 * Drop only when video is far behind audio (catch up without pacing). */
                {
                    double aclk = p->audio_clock;
                    if (!isnan(aclk) && aclk - pos > 0.15)
                        skip_store = 1;
                }
                pthread_mutex_unlock(&p->mu);

                if (!skip_store) {
                    if (p->use_hw && frame->format == p->hw_pix_fmt) {
                        sw = av_frame_alloc();
                        if (!sw || av_hwframe_transfer_data(sw, frame, 0) < 0) {
                            if (!xfer_err_logged) {
                                fprintf(stderr, "flux_ffmpeg: hwframe_transfer failed fmt=%d\n", frame->format);
                                xfer_err_logged = 1;
                            }
                            if (sw) av_frame_free(&sw);
                            av_frame_unref(frame);
                            continue;
                        }
                        use = sw;
                    }
                    store_frame(p, use, want_w, want_h);
                    frames_out++;
                    if (frames_out == 1) {
                        fprintf(stderr,
                                "flux_ffmpeg: first frame sw_fmt=%d %dx%d -> %dx%d\n",
                                use->format,
                                use->width,
                                use->height,
                                want_w,
                                want_h);
                    }
                    if (sw) av_frame_free(&sw);
                }

                /* Pace only when we actually presented a frame. If UI is behind
                 * (skip_store), drop without sleeping — catch up to realtime. */
                if (skip_store) {
                    av_frame_unref(frame);
                    continue;
                }

                /* Pace video to audio clock when present (sync audio), else wall clock. */
                if (audio_sink) {
                    double aclk;
                    pthread_mutex_lock(&p->mu);
                    aclk = p->audio_clock;
                    pthread_mutex_unlock(&p->mu);
                    if (!isnan(aclk)) {
                        if (pos > aclk + 0.08) {
                            int64_t delay = (int64_t)((pos - aclk) * 1000000.0);
                            /* Cap sleep so demux stays responsive, but allow enough
                             * headroom to avoid progressive lipsync drift. */
                            if (delay > 40000) delay = 40000;
                            if (delay > 1000) usleep((useconds_t)delay);
                        }
                    } else if (isnan(pts0)) {
                        pts0 = pos;
                        clock0 = av_gettime_relative();
                    } else {
                        int64_t target = clock0 + (int64_t)((pos - pts0) * 1000000.0);
                        int64_t now = av_gettime_relative();
                        if (target > now + 1000) {
                            int64_t delay = target - now;
                            if (delay > 100000) delay = 100000;
                            usleep((useconds_t)delay);
                        }
                    }
                } else if (isnan(pts0)) {
                    pts0 = pos;
                    clock0 = av_gettime_relative();
                } else {
                    int64_t target = clock0 + (int64_t)((pos - pts0) * 1000000.0);
                    int64_t now = av_gettime_relative();
                    if (target > now + 1000) {
                        int64_t delay = target - now;
                        if (delay > 100000) delay = 100000; /* cap 100ms */
                        usleep((useconds_t)delay);
                    } else if (now > target + 500000) {
                        /* Behind >0.5s — resync clock */
                        pts0 = pos;
                        clock0 = now;
                    }
                }
                av_frame_unref(frame);
            }
        }
    }

done:
    av_dict_free(&opts);
    if (aframe) av_frame_free(&aframe);
    if (frame) av_frame_free(&frame);
    if (pkt) av_packet_free(&pkt);
    if (swr) swr_free(&swr);
    if (actx) avcodec_free_context(&actx);
    if (audio_sink) pclose(audio_sink);
    if (vctx) avcodec_free_context(&vctx);
    if (fmt) avformat_close_input(&fmt);

    pthread_mutex_lock(&p->mu);
    p->alive = 0;
    pthread_mutex_unlock(&p->mu);
    fprintf(stderr, "flux_ffmpeg: decode loop exit\n");
    return NULL;
}
