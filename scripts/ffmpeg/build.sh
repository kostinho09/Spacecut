#!/bin/bash
# Builds Spacecut's own ffmpeg: sound only, from FFmpeg's official source.
# It holds what the app uses and nothing else:
#   - reading: Spaces' HLS over https, and local audio and video files (the
#     sound taken out; pictures are never decoded, apart from H.263, without
#     which 3gp and flv cuts came out a little longer than with a full ffmpeg)
#   - writing: m4a with AAC, the player's raw samples, MPEG-TS pieces
#   - filters: joining, speed, resampling, silence
# Floating point as a full build does it (-ffp-contract=off), so that every
# file comes out byte for byte as with the ffmpeg it replaced.
#
# Use:
#   scripts/ffmpeg/build.sh mac <output file>       on a Mac with Apple silicon
#   scripts/ffmpeg/build.sh windows <output file>   in MSYS2's MINGW64 shell
set -euo pipefail

VERSION=8.0
SHA256=b2751fccb6cc4c77708113cd78b561059b6fa904b24162fa0be2d60273d27b8e

platform=${1:?mac or windows}
output=${2:?where to put the ffmpeg built}
case $output in /*|[A-Za-z]:*) ;; *) output="$PWD/$output" ;; esac
work=${FFMPEG_WORK:-$(mktemp -d)}
mkdir -p "$work"
cd "$work"

# The source, checked against the one tested
if [ ! -f "ffmpeg-$VERSION.tar.xz" ]; then
  curl -sSfL -o "ffmpeg-$VERSION.tar.xz" "https://ffmpeg.org/releases/ffmpeg-$VERSION.tar.xz"
fi
echo "$SHA256  ffmpeg-$VERSION.tar.xz" | shasum -a 256 -c - 2>/dev/null || echo "$SHA256  ffmpeg-$VERSION.tar.xz" | sha256sum -c -
rm -rf "ffmpeg-$VERSION"
tar xf "ffmpeg-$VERSION.tar.xz"
cd "ffmpeg-$VERSION"

common=(
  --disable-everything --disable-autodetect --disable-doc --disable-debug
  --disable-ffplay --disable-ffprobe --disable-swscale
  --enable-network --enable-zlib
  --enable-avdevice --enable-indev=lavfi
  --enable-protocol=file,pipe,http,https,tcp,tls,crypto
  --enable-demuxer=dts,mpegvideo,h264,hevc,m4v,h263,hls,mpegts,aac,mov,matroska,avi,flv,asf,mpegps,mp3,wav,w64,flac,ogg,aiff,amr
  --enable-decoder=h263,flv,dca,aac,aac_latm,mp3,mp3float,mp2,mp2float,mp1,mp1float,ac3,eac3,opus,vorbis,flac,alac,wmav1,wmav2,amrnb,amrwb,pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_s32be,pcm_f32le,pcm_f32be,pcm_u8,pcm_alaw,pcm_mulaw
  --enable-encoder=aac,pcm_f32le,pcm_s16le
  --enable-muxer=ipod,mp4,mov,adts,mpegts,pcm_f32le,wav,framemd5,null
  --enable-parser=dca,mpegvideo,h264,hevc,mpeg4video,h263,aac,aac_latm,mpegaudio,ac3,opus,vorbis,flac
  --enable-bsf=aac_adtstoasc
  --enable-filter=abuffer,abuffersink,aresample,aformat,anull,atrim,asetpts,concat,atempo,anullsrc,volume,amix,apad,asplit,null,nullsink
)

case $platform in
  mac)
    ./configure "${common[@]}" --arch=arm64 --cc=clang \
      --extra-cflags="-fno-stack-check -mmacosx-version-min=11.0 -ffp-contract=off" \
      --extra-ldflags=-mmacosx-version-min=11.0 \
      --enable-securetransport
    make -j"$(sysctl -n hw.ncpu)"
    exe=ffmpeg
    ;;
  windows)
    # One .exe needing only Windows' own DLLs; https through Windows' own TLS
    ./configure "${common[@]}" --arch=x86_64 --target-os=mingw32 \
      --extra-cflags=-ffp-contract=off --extra-ldflags=-static --pkg-config-flags=--static \
      --enable-schannel --enable-w32threads
    make -j"$(nproc)"
    exe=ffmpeg.exe
    ;;
  *) echo "mac or windows"; exit 1 ;;
esac

mkdir -p "$(dirname "$output")"
cp "$exe" "$output"
"$output" -hide_banner -version | head -1
echo "Built: $output ($(wc -c < "$output" | tr -d ' ') bytes)"
