#!/bin/sh
# Software-encoder throughput on this Mac with low-latency settings: can it keep up with 60 fps, and what does it cost?
# Usage: sh bench_encoders.sh        (needs ffmpeg with libsvtav1/libvpx-vp9/libaom, e.g. Homebrew)
# "x realtime" is encoded frames per second / 60; it must stay well above 1.0 with headroom for the capture,
# the other display and the rest of macOS. cpu% is user+sys CPU time over wall time (100 = one core).
export PATH=/opt/homebrew/bin:$PATH
T=${T:-8}   # seconds of 60 fps video per run

run() { # name size [ffmpeg encoder args...]
  name=$1; size=$2; shift 2
  out=$(ffmpeg -hide_banner -nostdin -benchmark -f lavfi -i "testsrc2=size=$size:rate=60" -t "$T" "$@" -f null - 2>&1)
  fps=$(printf '%s\n' "$out" | tr '\r' '\n' | grep -o 'fps= *[0-9.]*' | tail -1 | tr -d 'fps= ')
  ut=$(printf '%s\n' "$out" | grep -o 'utime=[0-9.]*' | tail -1 | cut -d= -f2 | tr -d s)
  st=$(printf '%s\n' "$out" | grep -o 'stime=[0-9.]*' | tail -1 | cut -d= -f2 | tr -d s)
  rt=$(printf '%s\n' "$out" | grep -o 'rtime=[0-9.]*' | tail -1 | cut -d= -f2 | tr -d s)
  awk -v n="$name" -v s="$size" -v f="${fps:-0}" -v u="${ut:-0}" -v t="${st:-0}" -v r="${rt:-1}" \
    'BEGIN { printf "%-34s %-10s %6.1f fps  %5.2f x realtime  cpu %4.0f%%\n", n, s, f, f/60, (u+t)/r*100 }'
}

for size in 2560x1440 1920x1080; do
  run "H.264 VideoToolbox (reference)" $size -c:v h264_videotoolbox -realtime 1 -b:v 30M -bf 0
  run "SVT-AV1 preset 10 rtc"        $size -c:v libsvtav1 -preset 10 -b:v 30M -svtav1-params "rtc=1:lookahead=0:tune=0"
  run "SVT-AV1 preset 12 rtc"        $size -c:v libsvtav1 -preset 12 -b:v 30M -svtav1-params "rtc=1:lookahead=0:tune=0"
  run "SVT-AV1 preset 13 rtc"        $size -c:v libsvtav1 -preset 13 -b:v 30M -svtav1-params "rtc=1:lookahead=0:tune=0"
  run "libaom AV1 realtime cpu-used 10" $size -c:v libaom-av1 -usage realtime -cpu-used 10 -row-mt 1 -tiles 2x1 -lag-in-frames 0 -b:v 30M
  run "libvpx VP9 realtime cpu-used 8"  $size -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -row-mt 1 -tile-columns 2 -lag-in-frames 0 -b:v 30M
  run "libvpx VP9 realtime cpu-used 6"  $size -c:v libvpx-vp9 -deadline realtime -cpu-used 6 -row-mt 1 -tile-columns 2 -lag-in-frames 0 -b:v 30M
done
