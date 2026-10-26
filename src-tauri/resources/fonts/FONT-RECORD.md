# Bundled font record — Liberation 2.1.5

This file is the audited record required by `docs/P2.7.6-embed-DESIGN.md` §4 and OFL
condition 2. It is checked by `src-tauri/tests/font_bundle.rs`: the per-face hashes below are
re-hashed from the committed bytes, so a face cannot be swapped, truncated or dropped without
a test failing.

## Provenance

| | |
|---|---|
| Upstream | Liberation Fonts **2.1.5**, tag commit `4b0192046158` |
| Source used | Debian `fonts-liberation` **2.1.5-3** (`all`), built from that tag |
| Package URL | `http://deb.debian.org/debian/pool/main/f/fonts-liberation/fonts-liberation_2.1.5-3_all.deb` |
| Package SHA-256 | `3d5c47480067b9615f6bf9b0eda6eefcab9d837af609827015445e8e759b5b8c` |
| Licence file | `LICENSE-Liberation.txt`, copied verbatim from the upstream 2.1.5 tag |
| Faces | 12, total **4,360,440 bytes** |

**Why the Debian package and not the upstream release page.** Measured at bundling time:
*every* upstream GitHub release from 2.1.1 to 2.1.5 has **zero attached assets**, and the
source tarball contains only `.sfd` files — no TTFs. The binaries are produced by FontForge
and are not published upstream. The Debian package is a versioned, checksummed artifact built
from the pinned tag, so it satisfies the design's actual requirement — "not whatever the build
machine happens to have installed" — without a FontForge build step. The faces extracted from
it are byte-identical to the ones the design measured (Sans-Regular 410,820 B, Serif-Regular
393,692 B, Mono-Regular 319,624 B).

Reproduce:

```
curl -sSLo fl.deb http://deb.debian.org/debian/pool/main/f/fonts-liberation/fonts-liberation_2.1.5-3_all.deb
sha256sum fl.deb   # must equal the hash above
dpkg-deb -x fl.deb out && cp out/usr/share/fonts/truetype/liberation/*.ttf src-tauri/resources/fonts/
```

## Per-face audit

Read from each face's own `name` table (`name` IDs 0, 1, 2, 4, 6, 13, 14). All twelve agree on
the notice and the licence identifier.

| File | Bytes | SHA-256 | PostScript name | `name` ID 13 |
|---|---|---|---|---|
| `LiberationMono-Bold.ttf` | 308,068 | `ddce09ee3483b2d5faf61f0baba8592e76e0ac6dd4ab36683e5334c288bf5eef` | `LiberationMono-Bold` | `Licensed under the SIL Open Font License, Version 1.1` |
| `LiberationMono-BoldItalic.ttf` | 284,184 | `2ec6f3e5bd9b24c7fb04c229a1ee64f600e4072c03a041c96c2d62974c2ede2c` | `LiberationMono-BoldItalic` | `Licensed under the SIL Open Font License, Version 1.1` |
| `LiberationMono-Italic.ttf` | 281,608 | `016743fc425a1b61eb349c72c02f3da6a1349d1da7a5d9aded27321397eed955` | `LiberationMono-Italic` | `Licensed under the SIL Open Font License, Version 1.1` |
| `LiberationMono-Regular.ttf` | 319,624 | `5883330d94debd992952cd8f0571b225f478c2d797d3f36c7521b0a5c9bde0f2` | `LiberationMono` | `Licensed under the SIL Open Font License, Version 1.1` |
| `LiberationSans-Bold.ttf` | 414,568 | `1b5f2da6f4cadce4c05b9ecebe3a6fcd374eb95ae443605e799f4c3287978939` | `LiberationSans-Bold` | `Licensed under the SIL Open Font License, Version 1.1` |
| `LiberationSans-BoldItalic.ttf` | 409,108 | `352e9dc2be89f210dbd3c0fd5dfaf97135de70e0beb5f22378601861462493a7` | `LiberationSans-BoldItalic` | `Licensed under the SIL Open Font License, Version 1.1` |
| `LiberationSans-Italic.ttf` | 415,920 | `93ff5260834141b6f9d8f2b846d427f3a5dc492dbdce8ebdad406e38a38f34bb` | `LiberationSans-Italic` | `Licensed under the SIL Open Font License, Version 1.1` |
| `LiberationSans-Regular.ttf` | 410,820 | `bade59d822652f76e6941aa87b40a87c13d1cc70db98ededb5011127efafd1d3` | `LiberationSans` | `Licensed under the SIL Open Font License, Version 1.1` |
| `LiberationSerif-Bold.ttf` | 370,196 | `443a4dbd694b6e4689925947b456e0532e10b19c90fa32eb39ea6c2cd9db9ebd` | `LiberationSerif-Bold` | `Licensed under the SIL Open Font License, Version 1.1` |
| `LiberationSerif-BoldItalic.ttf` | 376,892 | `4b2635d2ec4d992373dcae577c742a98ecfd841ffc1b8a103b5f10db163dbc7a` | `LiberationSerif-BoldItalic` | `Licensed under the SIL Open Font License, Version 1.1` |
| `LiberationSerif-Italic.ttf` | 375,760 | `de18fe26337e03241952dca46398d5b66f3c5738d080bfd284f66e67b926f597` | `LiberationSerif-Italic` | `Licensed under the SIL Open Font License, Version 1.1` |
| `LiberationSerif-Regular.ttf` | 393,692 | `9caef765d2e891c10dd73658894f01e660fe0c1c83e0bda7d6edf561d8f623d4` | `LiberationSerif` | `Licensed under the SIL Open Font License, Version 1.1` |

Every face carries:

- **ID 0 (copyright):** "Digitized data copyright (c) 2010 Google Corporation." and
  "Copyright (c) 2012 Red Hat, Inc." — the notice OFL condition 2 requires to travel with
  the font.
- **ID 13 (licence):** "Licensed under the SIL Open Font License, Version 1.1".
- **ID 14 (licence URL):** `http://scripts.sil.org/OFL`.
- No face declares the Reserved Font Name in its own `name` table. The RFN is declared in
  `LICENSE-Liberation.txt`: "with Reserved Font Name Liberation". See the ruling below — this
  is why the written font name is not `Liberation`.

## Reserved Font Name ruling

OFL condition 3: *"No Modified Version of the Font Software may use the Reserved Font Name(s)
unless explicit written permission is granted … This restriction only applies to the primary
font name as presented to the users."* The licence defines a Modified Version as a derivative
made by *"adding to, deleting, or substituting … any of the components"*.

**Subsetting deletes glyphs, so a subset is a Modified Version.** Writing it as
`/BaseFont /ABCDEF+LiberationSans` would present the reserved name as the primary font name
and breach condition 3. The frozen design's §2.2 spelled the name that way; that spelling is
**corrected here** (see the design's §4 amendment).

What the write path does instead:

- The written `/BaseFont` uses a name that does not contain `Liberation`:
  `/BaseFont /<PREFIX>+<Family>-<Style>` with a family token from the table below,
  e.g. `/ABCDEF+Sans-Regular`.
- The subset's `name` table keeps the copyright and licence records (IDs 0, 13, 14) and does
  **not** carry the reserved family name as ID 1/4/6.
- Unmodified whole faces are never written into a PDF, so condition 2's "each copy contains
  the notice" is satisfied by shipping `LICENSE-Liberation.txt` in the installer and showing
  it in About (below), which is what the bundled copy *is*.

| Style class | Bundled face | Written family token |
|---|---|---|
| Helvetica | LiberationSans-Regular | `Sans-Regular` |
| Helvetica-Bold | LiberationSans-Bold | `Sans-Bold` |
| Helvetica-Oblique | LiberationSans-Italic | `Sans-Italic` |
| Helvetica-BoldOblique | LiberationSans-BoldItalic | `Sans-BoldItalic` |
| Times-Roman | LiberationSerif-Regular | `Serif-Regular` |
| Times-Bold | LiberationSerif-Bold | `Serif-Bold` |
| Times-Italic | LiberationSerif-Italic | `Serif-Italic` |
| Times-BoldItalic | LiberationSerif-BoldItalic | `Serif-BoldItalic` |
| Courier | LiberationMono-Regular | `Mono-Regular` |
| Courier-Bold | LiberationMono-Bold | `Mono-Bold` |
| Courier-Oblique | LiberationMono-Italic | `Mono-Italic` |
| Courier-BoldOblique | LiberationMono-BoldItalic | `Mono-BoldItalic` |

**Not legal advice.** It is the reading this project implements, stated so a later change has
to argue with it rather than silently undo it.
