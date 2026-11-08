# Third-party software

Spacecut, Copyright (C) 2026 kostinho09, is licensed under the GNU General
Public License, version 3 or later (see `LICENSE`).

To read, cut and join audio it runs **FFmpeg** as a separate program. FFmpeg is
not part of Spacecut's code. The builds shipped next to the app, kept in
`ffmpeg/mac` and `ffmpeg/windows`, are Spacecut's own, sound only: made from
FFmpeg's official source by `scripts/ffmpeg/build.sh`, which lists every
option they were built with.

| Platform | FFmpeg build | FFmpeg's license |
|---|---|---|
| macOS (Apple silicon) | FFmpeg 8.0, `scripts/ffmpeg/build.sh mac` | LGPL version 2.1 or later |
| Windows (x64) | FFmpeg 8.0, `scripts/ffmpeg/build.sh windows` (MSYS2, MinGW-w64) | LGPL version 2.1 or later |

FFmpeg is free software by the FFmpeg developers, https://ffmpeg.org. The
source both builds are made from is
https://ffmpeg.org/releases/ffmpeg-8.0.tar.xz (SHA-256
`b2751fccb6cc4c77708113cd78b561059b6fa904b24162fa0be2d60273d27b8e`), also at
https://github.com/FFmpeg/FFmpeg (tag `n8.0`).

The Rust libraries Spacecut is built from are under permissive licenses
(MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0, Unicode-3.0, CDLA-Permissive-2.0),
all compatible with the GPL version 3.
