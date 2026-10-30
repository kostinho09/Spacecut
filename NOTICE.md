# Third-party software

Space Cutter, Copyright (C) 2026 kostinho09, is licensed under the GNU General
Public License, version 3 or later (see `LICENSE`).

To read, cut and join audio it runs **FFmpeg** as a separate program. FFmpeg is
not part of Space Cutter's code: the ready-made builds below are shipped next to
the app exactly as they were downloaded; they are kept in `ffmpeg/mac` and
`ffmpeg/windows`.

| Platform | FFmpeg build | FFmpeg's license |
|---|---|---|
| macOS (Apple silicon) | FFmpeg 8.0, built with `--enable-gpl` | GPL version 2 or later |
| Windows (x64) | `2026-02-26-git-6695528af6-essentials_build` from gyan.dev | GPL version 3 |

FFmpeg is free software by the FFmpeg developers, https://ffmpeg.org. Its
source code is at https://ffmpeg.org/download.html and
https://github.com/FFmpeg/FFmpeg (the Windows build is commit `6695528af6`).

The Rust libraries Space Cutter is built from are under permissive licenses
(MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0, Unicode-3.0, CDLA-Permissive-2.0),
all compatible with the GPL version 3.
