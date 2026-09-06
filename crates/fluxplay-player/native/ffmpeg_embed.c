/* Embedded FFmpeg player — demux + decode video to RGBA for iced software stage.
 * Audio is decoded and discarded for now (video embed priority); volume/pause still tracked.
 */
#include "ffmpeg_embed.h"

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/avutil.h>
#include <libavutil/hwcontext.h>
#include <libavutil/imgutils.h>
#include <libavutil/opt.h>
#include <libavutil/time.h>
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

    uint8_t *frame_rgba;
    int frame_w;
    int frame_h;
    int frame_ready;
    int want_w;
    int want_h;
    struct SwsContext *sws;
    AVBufferRef *hw_device_ctx;
    enum AVPixelFormat hw_pix_fmt;
    int use_hw;

    char *url;
    char *user_agent;
    char *referer;
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
    p->low_latency = opts->low_latency;
    p->volume = 1.0f;
    p->alive = 1;
    p->want_w = 960;
    p->want_h = 540;

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
    /* Size mismatch: drop stale frame so decode can emit at want_w×want_h.
     * Leaving frame_ready=1 caused a deadlock (UI never consumes, decode skip_store forever → black stage). */
    if (p->frame_w != out_w || p->frame_h != out_h) {
        p->frame_ready = 0;
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
    int r = p->frame_ready;
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
    uint8_t *dst = (uint8_t *)malloc(need);
    if (!dst) {
        return;
    }
    uint8_t *dst_slices[4] = {dst, NULL, NULL, NULL};
    int dst_stride[4] = {tw * 4, 0, 0, 0};
    sws_scale(sws, (const uint8_t *const *)frame->data, frame->linesize, 0, frame->height, dst_slices, dst_stride);

    pthread_mutex_lock(&p->mu);
    free(p->frame_rgba);
    p->frame_rgba = dst;
    p->frame_w = tw;
    p->frame_h = th;
    p->frame_ready = 1;
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

static void *decode_thread(void *arg) {
    FluxFfmpegPlayer *p = (FluxFfmpegPlayer *)arg;
    AVFormatContext *fmt = NULL;
    AVCodecContext *vctx = NULL;
    AVPacket *pkt = NULL;
    AVFrame *frame = NULL;
    int vindex = -1;
    int want_w = 960;
    int want_h = 540;

    AVDictionary *opts = NULL;
    if (p->user_agent) av_dict_set(&opts, "user_agent", p->user_agent, 0);
    else av_dict_set(&opts, "user_agent", "IPTVSmartersPlayer", 0);
    if (p->referer) {
        char hdr[1024];
        snprintf(hdr, sizeof(hdr), "Referer: %s\r\n", p->referer);
        av_dict_set(&opts, "headers", hdr, 0);
    }
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
        try_init_hw(p, codec, vctx);
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

    pkt = av_packet_alloc();
    frame = av_frame_alloc();
    if (!pkt || !frame) goto done;

    fprintf(stderr, "flux_ffmpeg: decode loop start\n");

    int64_t clock0 = av_gettime_relative();
    double pts0 = NAN;
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
        want_w = p->want_w >= 2 ? p->want_w : 960;
        want_h = p->want_h >= 2 ? p->want_h : 540;
        pthread_mutex_unlock(&p->mu);
        if (stop) break;

        if (seek_req) {
            int64_t ts = (int64_t)(seek_secs * AV_TIME_BASE);
            av_seek_frame(fmt, -1, ts, AVSEEK_FLAG_BACKWARD);
            avcodec_flush_buffers(vctx);
            pthread_mutex_lock(&p->mu);
            p->seek_req = 0;
            p->position = seek_secs;
            pthread_mutex_unlock(&p->mu);
            pts0 = NAN;
            clock0 = av_gettime_relative();
        }

        if (paused) {
            usleep(20000);
            clock0 = av_gettime_relative();
            pts0 = NAN;
            continue;
        }

        int r = av_read_frame(fmt, pkt);
        if (r < 0) {
            if (r == AVERROR_EOF) break;
            usleep(10000);
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
                /* If UI still holds an unread frame, drop this one (keeps realtime). */
                if (p->frame_ready) skip_store = 1;
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

                /* Pace to media clock so we don't melt the CPU decoding ASAP. */
                if (isnan(pts0)) {
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
    if (frame) av_frame_free(&frame);
    if (pkt) av_packet_free(&pkt);
    if (vctx) avcodec_free_context(&vctx);
    if (fmt) avformat_close_input(&fmt);

    pthread_mutex_lock(&p->mu);
    p->alive = 0;
    pthread_mutex_unlock(&p->mu);
    fprintf(stderr, "flux_ffmpeg: decode loop exit\n");
    return NULL;
}
