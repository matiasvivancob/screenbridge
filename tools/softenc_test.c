// Standalone check of mac/Sources/softenc.c on a Mac: encodes synthetic 1440p desktop-like NV12 frames with AV1 and VP9,
// prints per-frame latency and size statistics, and writes the streams so ffmpeg can prove they decode.
//
//   clang -O2 -DSB_HAVE_SOFTENC -I/opt/homebrew/include -DSB_SVT_LIB='"libSvtAv1Enc.4.dylib"' \
//         -DSB_VPX_LIB='"libvpx.12.dylib"' mac/Sources/softenc.c tools/softenc_test.c -o /tmp/softenc_test
//   /tmp/softenc_test && ffmpeg -v error -i /tmp/sb_av1.ivf -f null - && ffmpeg -v error -i /tmp/sb_vp9.ivf -f null -
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "../mac/Sources/softenc.h"

static double now_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1e3 + t.tv_nsec / 1e6;
}

static int cmp(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return (x > y) - (x < y);
}

static void le16(unsigned char *p, int v) { p[0] = v; p[1] = v >> 8; }
static void le32(unsigned char *p, unsigned v) { p[0] = v; p[1] = v >> 8; p[2] = v >> 16; p[3] = v >> 24; }

/// Desktop-like picture: flat background, a window, "text" lines, and a bar that moves every frame.
static void paint(unsigned char *y, unsigned char *uv, int w, int h, int ys, int frame) {
    for (int r = 0; r < h; r++) {
        unsigned char *row = y + (size_t)r * ys;
        for (int x = 0; x < w; x++) {
            int v = 235;
            if (x > w / 8 && x < w * 7 / 8 && r > h / 8 && r < h * 7 / 8) v = 245;
            if (r > h / 4 && r < h * 3 / 4 && ((r / 18) % 2) == 0 && ((x / 7 + r / 18) % 5) != 0) v = 40;
            int bx = (frame * 24) % w;
            if (x >= bx && x < bx + 64 && r > h * 7 / 8) v = 30;
            row[x] = (unsigned char)v;
        }
    }
    for (int r = 0; r < h / 2; r++) {
        unsigned char *row = uv + (size_t)r * ys;
        for (int x = 0; x < w / 2; x++) {
            row[2 * x] = 128;
            row[2 * x + 1] = (unsigned char)(120 + ((x + frame) / 40 % 2) * 16);
        }
    }
}

static int run(int codec, const char *name, const char *path, int w, int h, int frames) {
    char err[200] = {0};
    if (!sb_soft_available(codec)) {
        printf("%-4s not available (library did not load)\n", name);
        return 1;
    }
    SBSoftEncoder *e = sb_soft_create(codec, w, h, 60, 30000, err, sizeof err);
    if (!e) {
        printf("%-4s create failed: %s\n", name, err);
        return 1;
    }
    int ys = w;
    unsigned char *y = malloc((size_t)ys * h), *uv = malloc((size_t)ys * h / 2);
    unsigned char *out = malloc(4 << 20);
    FILE *f = fopen(path, "wb");
    char keys_path[300];
    snprintf(keys_path, sizeof keys_path, "%s.keys", path);
    FILE *kf = fopen(keys_path, "w"); // indices of the keyframes, for tools/replay_ivf.py
    { // IVF container (both codecs) so ffmpeg can read it and frame boundaries are kept
        unsigned char hdr[32] = {'D', 'K', 'I', 'F'};
        le16(hdr + 4, 0); le16(hdr + 6, 32);
        memcpy(hdr + 8, codec == SB_CODEC_VP9 ? "VP90" : "AV01", 4);
        le16(hdr + 12, w); le16(hdr + 14, h); le32(hdr + 16, 60); le32(hdr + 20, 1); le32(hdr + 24, frames);
        fwrite(hdr, 1, 32, f);
    }
    double *ms = malloc(sizeof(double) * frames);
    int n_ms = 0, keys = 0, produced = 0;
    long total = 0, key_bytes = 0, max_frame = 0;
    for (int i = 0; i < frames; i++) {
        paint(y, uv, w, h, ys, i);
        int is_key = 0;
        double t0 = now_ms();
        int n = sb_soft_encode(e, y, ys, uv, ys, i == 0 || i == 100, out, 4 << 20, &is_key);
        double t1 = now_ms();
        if (n < 0) { printf("%-4s encode error %d at frame %d\n", name, n, i); return 1; }
        if (n > 0) {
            produced++;
            total += n;
            if (n > max_frame) max_frame = n;
            if (is_key) { keys++; key_bytes += n; }
            if (is_key) fprintf(kf, "%d\n", produced - 1);
            {
                unsigned char fh[12];
                le32(fh, n); le32(fh + 4, i); le32(fh + 8, 0);
                fwrite(fh, 1, 12, f);
            }
            fwrite(out, 1, n, f);
        }
        if (i >= 10) ms[n_ms++] = t1 - t0;
    }
    fclose(f);
    fclose(kf);
    qsort(ms, n_ms, sizeof(double), cmp);
    printf("%-4s %dx%d  %d/%d packets  keyframes %d (%.0f KB each)  avg %.1f KB/frame  max %.0f KB   "
           "encode p50 %.1f ms  p95 %.1f ms  max %.1f ms\n",
           name, w, h, produced, frames, keys, keys ? key_bytes / keys / 1024.0 : 0.0, total / 1024.0 / (produced ? produced : 1),
           max_frame / 1024.0, ms[n_ms / 2], ms[(int)(n_ms * 0.95)], ms[n_ms - 1]);
    sb_soft_destroy(e);
    free(y); free(uv); free(out); free(ms);
    return 0;
}

int main(void) {
    int bad = 0;
    bad += run(SB_CODEC_AV1, "AV1", "/tmp/sb_av1.ivf", 2560, 1440, 180);
    bad += run(SB_CODEC_VP9, "VP9", "/tmp/sb_vp9.ivf", 2560, 1440, 180);
    bad += run(SB_CODEC_AV1, "AV1", "/tmp/sb_av1_1080.ivf", 1920, 1080, 180);
    bad += run(SB_CODEC_VP9, "VP9", "/tmp/sb_vp9_1080.ivf", 1920, 1080, 180);
    return bad;
}
