// See softenc.h. Built with -DSB_HAVE_SOFTENC (and the SVT-AV1 / libvpx headers) for the arm64 slice when the
// libraries are installed; otherwise this file is only the stub at the bottom.
#include "softenc.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(SB_HAVE_SOFTENC)

#include <dlfcn.h>
#include <pthread.h>
#include <svt-av1/EbSvtAv1Enc.h>
#include <vpx/vp8cx.h>
#include <vpx/vpx_encoder.h>

// The libraries are loaded at run time (dlopen), not linked: an app that bundles them still launches on a Mac where
// they cannot load (e.g. built for a newer macOS), and AV1/VP9 are then simply unavailable.
#ifndef SB_SVT_LIB
#define SB_SVT_LIB "libSvtAv1Enc.4.dylib"
#endif
#ifndef SB_VPX_LIB
#define SB_VPX_LIB "libvpx.12.dylib"
#endif

#define SB_FN(name) static __typeof__(&name) p_##name;
SB_FN(svt_av1_enc_init_handle)
SB_FN(svt_av1_enc_set_parameter)
SB_FN(svt_av1_enc_parse_parameter)
SB_FN(svt_av1_enc_init)
SB_FN(svt_av1_enc_send_picture)
SB_FN(svt_av1_enc_get_packet)
SB_FN(svt_av1_enc_release_out_buffer)
SB_FN(svt_av1_enc_deinit)
SB_FN(svt_av1_enc_deinit_handle)
SB_FN(vpx_codec_vp9_cx)
SB_FN(vpx_codec_enc_config_default)
SB_FN(vpx_codec_enc_config_set)
SB_FN(vpx_codec_enc_init_ver)
SB_FN(vpx_img_wrap)
SB_FN(vpx_codec_encode)
SB_FN(vpx_codec_get_cx_data)
SB_FN(vpx_codec_destroy)
static vpx_codec_err_t (*p_vpx_codec_control_)(vpx_codec_ctx_t *, int, ...);

// From here on the vendor names refer to the loaded function pointers.
#define svt_av1_enc_init_handle p_svt_av1_enc_init_handle
#define svt_av1_enc_set_parameter p_svt_av1_enc_set_parameter
#define svt_av1_enc_parse_parameter p_svt_av1_enc_parse_parameter
#define svt_av1_enc_init p_svt_av1_enc_init
#define svt_av1_enc_send_picture p_svt_av1_enc_send_picture
#define svt_av1_enc_get_packet p_svt_av1_enc_get_packet
#define svt_av1_enc_release_out_buffer p_svt_av1_enc_release_out_buffer
#define svt_av1_enc_deinit p_svt_av1_enc_deinit
#define svt_av1_enc_deinit_handle p_svt_av1_enc_deinit_handle
#define vpx_codec_vp9_cx p_vpx_codec_vp9_cx
#define vpx_codec_enc_config_default p_vpx_codec_enc_config_default
#define vpx_codec_enc_config_set p_vpx_codec_enc_config_set
#define vpx_codec_enc_init_ver p_vpx_codec_enc_init_ver
#define vpx_img_wrap p_vpx_img_wrap
#define vpx_codec_encode p_vpx_codec_encode
#define vpx_codec_get_cx_data p_vpx_codec_get_cx_data
#define vpx_codec_destroy p_vpx_codec_destroy

static int have_av1, have_vp9;
static pthread_once_t load_once_ctl = PTHREAD_ONCE_INIT;

static void *open_lib(const char *name) {
    char path[512];
    snprintf(path, sizeof path, "@rpath/%s", name);
    void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!h) {
        snprintf(path, sizeof path, "/opt/homebrew/lib/%s", name);
        h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    }
    return h;
}

#define SB_LOAD(lib, name) ((p_##name = (__typeof__(p_##name))dlsym(lib, #name)) != NULL)

static void load_libs(void) {
    void *s = open_lib(SB_SVT_LIB);
    if (s) {
        have_av1 = SB_LOAD(s, svt_av1_enc_init_handle) && SB_LOAD(s, svt_av1_enc_set_parameter) &&
                   SB_LOAD(s, svt_av1_enc_parse_parameter) && SB_LOAD(s, svt_av1_enc_init) &&
                   SB_LOAD(s, svt_av1_enc_send_picture) && SB_LOAD(s, svt_av1_enc_get_packet) &&
                   SB_LOAD(s, svt_av1_enc_release_out_buffer) && SB_LOAD(s, svt_av1_enc_deinit) &&
                   SB_LOAD(s, svt_av1_enc_deinit_handle);
    }
    void *v = open_lib(SB_VPX_LIB);
    if (v) {
        have_vp9 = SB_LOAD(v, vpx_codec_vp9_cx) && SB_LOAD(v, vpx_codec_enc_config_default) &&
                   SB_LOAD(v, vpx_codec_enc_config_set) && SB_LOAD(v, vpx_codec_enc_init_ver) &&
                   SB_LOAD(v, vpx_img_wrap) && SB_LOAD(v, vpx_codec_encode) && SB_LOAD(v, vpx_codec_get_cx_data) &&
                   SB_LOAD(v, vpx_codec_destroy);
        p_vpx_codec_control_ = (vpx_codec_err_t(*)(vpx_codec_ctx_t *, int, ...))dlsym(v, "vpx_codec_control_");
        have_vp9 = have_vp9 && p_vpx_codec_control_ != NULL;
    }
}

struct SBSoftEncoder {
    int codec, w, h, fps, kbps;
    uint8_t *u, *v; // I420 chroma planes built from the NV12 interleaved plane
    int64_t pts;
    // AV1 (SVT-AV1)
    EbComponentType *svt;
    EbSvtAv1EncConfiguration svt_cfg;
    // VP9 (libvpx)
    vpx_codec_ctx_t vpx;
    vpx_codec_enc_cfg_t vpx_cfg;
    int vpx_open;
};

static void set_err(char *err, size_t n, const char *msg) {
    if (err && n) snprintf(err, n, "%s", msg);
}

int sb_soft_available(int codec) {
    pthread_once(&load_once_ctl, load_libs);
    return (codec == SB_CODEC_AV1 && have_av1) || (codec == SB_CODEC_VP9 && have_vp9);
}

static int vp9_ctl(SBSoftEncoder *e, int id, int value) { return p_vpx_codec_control_(&e->vpx, id, value); }

static int av1_open(SBSoftEncoder *e, char *err, size_t err_len) {
    EbSvtAv1EncConfiguration *c = &e->svt_cfg;
    memset(c, 0, sizeof *c);
    if (svt_av1_enc_init_handle(&e->svt, c) != EB_ErrorNone) {
        set_err(err, err_len, "SVT-AV1: could not create the encoder");
        return -1;
    }
    c->source_width = (uint32_t)e->w;
    c->source_height = (uint32_t)e->h;
    c->frame_rate_numerator = (uint32_t)e->fps;
    c->frame_rate_denominator = 1;
    c->encoder_bit_depth = 8;
    c->enc_mode = 12;              // measured on an M1 Pro: preset 12 + rtc = 2.2x real time at 1440p, ~2.7 cores
    c->rate_control_mode = 2;      // CBR
    c->target_bit_rate = (uint32_t)e->kbps * 1000u;
    c->intra_period_length = -1;   // keyframes only on request
    c->rtc = true;                 // real-time communication tuning: low delay, no lookahead
    c->look_ahead_distance = 0;
    // Names (not struct fields) for options whose field names changed between releases.
    svt_av1_enc_parse_parameter(c, "pred-struct", "1"); // low delay: also makes get_packet blocking = one packet per picture
    if (svt_av1_enc_set_parameter(e->svt, c) != EB_ErrorNone) {
        set_err(err, err_len, "SVT-AV1: rejected the encoder settings");
        return -1;
    }
    if (svt_av1_enc_init(e->svt) != EB_ErrorNone) {
        set_err(err, err_len, "SVT-AV1: could not start the encoder");
        return -1;
    }
    return 0;
}

static int vp9_open(SBSoftEncoder *e, char *err, size_t err_len) {
    vpx_codec_iface_t *iface = vpx_codec_vp9_cx();
    if (vpx_codec_enc_config_default(iface, &e->vpx_cfg, 0) != VPX_CODEC_OK) {
        set_err(err, err_len, "libvpx: no default configuration");
        return -1;
    }
    vpx_codec_enc_cfg_t *c = &e->vpx_cfg;
    c->g_w = (unsigned)e->w;
    c->g_h = (unsigned)e->h;
    c->g_timebase.num = 1;
    c->g_timebase.den = e->fps;
    c->g_lag_in_frames = 0;        // no lookahead: the frame goes out as soon as it is encoded
    c->g_threads = 4;
    c->g_error_resilient = 0;
    c->rc_end_usage = VPX_CBR;
    c->rc_target_bitrate = (unsigned)e->kbps;
    c->rc_min_quantizer = 2;
    c->rc_max_quantizer = 52;
    c->rc_buf_initial_sz = 100;    // ms: small buffer = low delay
    c->rc_buf_optimal_sz = 150;
    c->rc_buf_sz = 250;
    c->kf_mode = VPX_KF_DISABLED;  // keyframes only on request
    if (vpx_codec_enc_init_ver(&e->vpx, iface, c, 0, VPX_ENCODER_ABI_VERSION) != VPX_CODEC_OK) {
        set_err(err, err_len, "libvpx: could not start the VP9 encoder");
        return -1;
    }
    e->vpx_open = 1;
    vp9_ctl(e, VP8E_SET_CPUUSED, 8);            // measured: 2.8x real time at 1440p, ~3.7 cores
    vp9_ctl(e, VP9E_SET_ROW_MT, 1);
    vp9_ctl(e, VP9E_SET_TILE_COLUMNS, 2);
    vp9_ctl(e, VP9E_SET_AQ_MODE, 3);            // cyclic refresh: suits mostly-static desktops
    vp9_ctl(e, VP9E_SET_NOISE_SENSITIVITY, 0);
    vp9_ctl(e, VP9E_SET_TUNE_CONTENT, VP9E_CONTENT_SCREEN);
    return 0;
}

SBSoftEncoder *sb_soft_create(int codec, int width, int height, int fps, int kbps, char *err, size_t err_len) {
    if (!sb_soft_available(codec) || width < 64 || height < 64 || (width & 1) || (height & 1)) {
        set_err(err, err_len, "unsupported codec or size");
        return NULL;
    }
    SBSoftEncoder *e = calloc(1, sizeof *e);
    if (!e) return NULL;
    e->codec = codec;
    e->w = width;
    e->h = height;
    e->fps = fps > 0 ? fps : 60;
    e->kbps = kbps > 0 ? kbps : 30000;
    e->u = malloc((size_t)(width / 2) * (height / 2));
    e->v = malloc((size_t)(width / 2) * (height / 2));
    int r = (e->u && e->v) ? (codec == SB_CODEC_AV1 ? av1_open(e, err, err_len) : vp9_open(e, err, err_len)) : -1;
    if (r != 0) {
        sb_soft_destroy(e);
        return NULL;
    }
    return e;
}

void sb_soft_destroy(SBSoftEncoder *e) {
    if (!e) return;
    if (e->svt) {
        svt_av1_enc_deinit(e->svt);
        svt_av1_enc_deinit_handle(e->svt);
    }
    if (e->vpx_open) vpx_codec_destroy(&e->vpx);
    free(e->u);
    free(e->v);
    free(e);
}

int sb_soft_set_bitrate(SBSoftEncoder *e, int kbps) {
    if (!e || kbps <= 0) return -1;
    e->kbps = kbps;
    if (e->codec == SB_CODEC_VP9) {
        e->vpx_cfg.rc_target_bitrate = (unsigned)kbps;
        return vpx_codec_enc_config_set(&e->vpx, &e->vpx_cfg) == VPX_CODEC_OK ? 0 : -1;
    }
    return -2; // SVT-AV1: the caller recreates the encoder instead
}

/// De-interleaves NV12's UV plane into separate U and V planes.
static void split_uv(const uint8_t *uv, int uv_stride, uint8_t *u, uint8_t *v, int cw, int ch) {
    for (int row = 0; row < ch; row++) {
        const uint8_t *s = uv + (size_t)row * uv_stride;
        uint8_t *du = u + (size_t)row * cw, *dv = v + (size_t)row * cw;
        for (int x = 0; x < cw; x++) {
            du[x] = s[2 * x];
            dv[x] = s[2 * x + 1];
        }
    }
}

int sb_soft_encode(SBSoftEncoder *e, const uint8_t *y, int y_stride, const uint8_t *uv, int uv_stride,
                   int force_key, uint8_t *out, size_t out_cap, int *is_key) {
    if (!e || !y || !uv || !out) return -1;
    const int cw = e->w / 2, ch = e->h / 2;
    split_uv(uv, uv_stride, e->u, e->v, cw, ch);
    *is_key = 0;

    if (e->codec == SB_CODEC_AV1) {
        EbSvtIOFormat io;
        memset(&io, 0, sizeof io);
        io.luma = (uint8_t *)y;
        io.cb = e->u;
        io.cr = e->v;
        io.y_stride = (uint32_t)y_stride;
        io.cb_stride = (uint32_t)cw;
        io.cr_stride = (uint32_t)cw;
        EbBufferHeaderType hdr;
        memset(&hdr, 0, sizeof hdr);
        hdr.size = sizeof hdr;
        hdr.p_buffer = (uint8_t *)&io;
        hdr.n_filled_len = (uint32_t)(y_stride * e->h + 2 * cw * ch);
        hdr.pts = e->pts++;
        hdr.pic_type = force_key ? EB_AV1_KEY_PICTURE : EB_AV1_INVALID_PICTURE;
        if (svt_av1_enc_send_picture(e->svt, &hdr) != EB_ErrorNone) return -2;
        EbBufferHeaderType *pkt = NULL;
        EbErrorType r = svt_av1_enc_get_packet(e->svt, &pkt, 0); // blocking in low-delay mode: one packet per picture
        if (r == EB_NoErrorEmptyQueue || !pkt) return 0;
        if (r != EB_ErrorNone) return -3;
        int n = (int)pkt->n_filled_len;
        if ((size_t)n > out_cap) {
            svt_av1_enc_release_out_buffer(&pkt);
            return -4;
        }
        memcpy(out, pkt->p_buffer, (size_t)n);
        *is_key = pkt->pic_type == EB_AV1_KEY_PICTURE || pkt->pic_type == EB_AV1_FW_KEY_PICTURE;
        svt_av1_enc_release_out_buffer(&pkt);
        return n;
    }

    vpx_image_t img;
    memset(&img, 0, sizeof img);
    vpx_img_wrap(&img, VPX_IMG_FMT_I420, (unsigned)e->w, (unsigned)e->h, 1, (unsigned char *)y);
    img.planes[VPX_PLANE_Y] = (unsigned char *)y;
    img.stride[VPX_PLANE_Y] = y_stride;
    img.planes[VPX_PLANE_U] = e->u;
    img.stride[VPX_PLANE_U] = cw;
    img.planes[VPX_PLANE_V] = e->v;
    img.stride[VPX_PLANE_V] = cw;
    vpx_enc_frame_flags_t flags = force_key ? VPX_EFLAG_FORCE_KF : 0;
    if (vpx_codec_encode(&e->vpx, &img, e->pts++, 1, flags, VPX_DL_REALTIME) != VPX_CODEC_OK) return -2;
    int total = 0;
    vpx_codec_iter_t it = NULL;
    const vpx_codec_cx_pkt_t *p;
    while ((p = vpx_codec_get_cx_data(&e->vpx, &it)) != NULL) {
        if (p->kind != VPX_CODEC_CX_FRAME_PKT) continue;
        if ((size_t)total + p->data.frame.sz > out_cap) return -4;
        memcpy(out + total, p->data.frame.buf, p->data.frame.sz);
        total += (int)p->data.frame.sz;
        if (p->data.frame.flags & VPX_FRAME_IS_KEY) *is_key = 1;
    }
    return total;
}

#else // ---- stub: libraries not available in this build ----

int sb_soft_available(int codec) { (void)codec; return 0; }
SBSoftEncoder *sb_soft_create(int codec, int width, int height, int fps, int kbps, char *err, size_t err_len) {
    (void)codec; (void)width; (void)height; (void)fps; (void)kbps;
    if (err && err_len) snprintf(err, err_len, "this build of ScreenBridge has no AV1/VP9 encoder");
    return NULL;
}
int sb_soft_encode(SBSoftEncoder *e, const uint8_t *y, int ys, const uint8_t *uv, int uvs, int fk, uint8_t *o, size_t oc, int *k) {
    (void)e; (void)y; (void)ys; (void)uv; (void)uvs; (void)fk; (void)o; (void)oc; (void)k;
    return -1;
}
int sb_soft_set_bitrate(SBSoftEncoder *e, int kbps) { (void)e; (void)kbps; return -1; }
void sb_soft_destroy(SBSoftEncoder *e) { (void)e; }

#endif
