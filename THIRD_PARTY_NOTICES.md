# Third-party notices

hf-mount is licensed under the Apache License 2.0 (see `LICENSE`). The Windows
edition uses or ships the third-party software below. Each package's own
license applies; the full per-package lists, generated from `cargo metadata`
and the npm lockfile by `scripts/windows/dependency-licenses.ps1`, ship with
the binaries:

* portable CLI archive: `licenses/rust-dependencies.txt` (every crate in
  `hf-mount.exe`'s dependency graph, from `Cargo.lock`);
* desktop installer: `licenses/rust-dependencies.txt` (every crate of the
  app, from `desktop/src-tauri/Cargo.lock`) and `licenses/npm-dependencies.txt`
  (every package bundled into the UI, from `desktop/package-lock.json`).

## WinFsp

**WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos**
<https://github.com/winfsp/winfsp>

hf-mount loads `winfsp-x64.dll` from the installed WinFsp at run time, and the
desktop installer ships the unmodified official WinFsp installer
(`winfsp-2.1.25156.msi`). WinFsp is licensed under the GPLv3 with a special
exception for Free/Libre and Open Source Software (FLOSS); hf-mount (Apache
License 2.0) uses WinFsp under that exception. The full license text is in
`licenses/WinFsp-License.txt`. hf-mount does not use the GPL-3.0 `winfsp` /
`winfsp-sys` Rust crates.

## Desktop app

| Component | License |
| --- | --- |
| [Tauri](https://tauri.app) 2 and its plugins (dialog, opener, autostart, clipboard-manager, single-instance) | Apache-2.0 OR MIT |
| [React](https://react.dev) and React DOM | MIT |
| [Radix UI primitives](https://www.radix-ui.com) | MIT |
| [Lucide](https://lucide.dev) icons (`lucide-react`) | ISC |
| [clsx](https://github.com/lukeed/clsx) | MIT |
| [Tailwind CSS](https://tailwindcss.com) (build time) | MIT |
| [Geist and Geist Mono](https://github.com/vercel/geist-font) fonts, via Fontsource | SIL Open Font License 1.1 (below) |
| Microsoft Edge WebView2 Runtime | Part of Windows; not redistributed. The installer only downloads Microsoft's bootstrapper if the runtime is missing. |

## Command line and engine

The engine is upstream hf-mount (Apache-2.0) and its dependencies, notably
[xet-core](https://github.com/huggingface/xet-core) (Apache-2.0),
[tokio](https://tokio.rs) (MIT), [reqwest](https://github.com/seanmonstar/reqwest)
(MIT OR Apache-2.0), [clap](https://github.com/clap-rs/clap) (MIT OR Apache-2.0)
and [windows-sys](https://github.com/microsoft/windows-rs) (MIT OR Apache-2.0).

## Geist and Geist Mono fonts

Copyright 2024 The Geist Project Authors (https://github.com/vercel/geist-font) Geist-Italic[wght].ttf: Copyright 2024 The Geist Project Authors (https://github.com/vercel/geist-font)

This Font Software is licensed under the SIL Open Font License, Version 1.1.
This license is copied below, and is also available with a FAQ at:
http://scripts.sil.org/OFL


-----------------------------------------------------------
SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007
-----------------------------------------------------------

PREAMBLE
The goals of the Open Font License (OFL) are to stimulate worldwide
development of collaborative font projects, to support the font creation
efforts of academic and linguistic communities, and to provide a free and
open framework in which fonts may be shared and improved in partnership
with others.

The OFL allows the licensed fonts to be used, studied, modified and
redistributed freely as long as they are not sold by themselves. The
fonts, including any derivative works, can be bundled, embedded,
redistributed and/or sold with any software provided that any reserved
names are not used by derivative works. The fonts and derivatives,
however, cannot be released under any other type of license. The
requirement for fonts to remain under this license does not apply
to any document created using the fonts or their derivatives.

DEFINITIONS
"Font Software" refers to the set of files released by the Copyright
Holder(s) under this license and clearly marked as such. This may
include source files, build scripts and documentation.

"Reserved Font Name" refers to any names specified as such after the
copyright statement(s).

"Original Version" refers to the collection of Font Software components as
distributed by the Copyright Holder(s).

"Modified Version" refers to any derivative made by adding to, deleting,
or substituting -- in part or in whole -- any of the components of the
Original Version, by changing formats or by porting the Font Software to a
new environment.

"Author" refers to any designer, engineer, programmer, technical
writer or other person who contributed to the Font Software.

PERMISSION & CONDITIONS
Permission is hereby granted, free of charge, to any person obtaining
a copy of the Font Software, to use, study, copy, merge, embed, modify,
redistribute, and sell modified and unmodified copies of the Font
Software, subject to the following conditions:

1) Neither the Font Software nor any of its individual components,
in Original or Modified Versions, may be sold by itself.

2) Original or Modified Versions of the Font Software may be bundled,
redistributed and/or sold with any software, provided that each copy
contains the above copyright notice and this license. These can be
included either as stand-alone text files, human-readable headers or
in the appropriate machine-readable metadata fields within text or
binary files as long as those fields can be easily viewed by the user.

3) No Modified Version of the Font Software may use the Reserved Font
Name(s) unless explicit written permission is granted by the corresponding
Copyright Holder. This restriction only applies to the primary font name as
presented to the users.

4) The name(s) of the Copyright Holder(s) or the Author(s) of the Font
Software shall not be used to promote, endorse or advertise any
Modified Version, except to acknowledge the contribution(s) of the
Copyright Holder(s) and the Author(s) or with their explicit written
permission.

5) The Font Software, modified or unmodified, in part or in whole,
must be distributed entirely under this license, and must not be
distributed under any other license. The requirement for fonts to
remain under this license does not apply to any document created
using the Font Software.

TERMINATION
This license becomes null and void if any of the above conditions are
not met.

DISCLAIMER
THE FONT SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO ANY WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT
OF COPYRIGHT, PATENT, TRADEMARK, OR OTHER RIGHT. IN NO EVENT SHALL THE
COPYRIGHT HOLDER BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
INCLUDING ANY GENERAL, SPECIAL, INDIRECT, INCIDENTAL, OR CONSEQUENTIAL
DAMAGES, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF THE USE OR INABILITY TO USE THE FONT SOFTWARE OR FROM
OTHER DEALINGS IN THE FONT SOFTWARE.

Geist Mono: Copyright 2024 The Geist Project Authors (https://github.com/vercel/geist-font.git) GeistMono-Italic[wght].ttf: Copyright 2024 The Geist Project Authors (https://github.com/vercel/geist-font.git)
Licensed under the same SIL Open Font License, Version 1.1.
