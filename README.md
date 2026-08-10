# pdftamp

A CLI tool that shrinks PDF file size — nothing else. It recompresses
embedded images and re-deflates uncompressed streams; it doesn't
touch fonts, page structure, metadata, or encryption unless you
explicitly ask it to.

## Why pdftamp?

Most PDF compression tools force you to choose between bloated file sizes and ruined document quality. `pdftamp` was built around a pragmatic **80/20 approach**: focus on optimizations that yield the biggest space savings without breaking your files or compromising readability.

### Core Goals & Philosophy

* **Quality-First Compression:** Maximize file size reduction while preserving visual fidelity as much as possible.
* **Non-Destructive Integrity:** Your PDFs should remain valid, clean, and fully functional—no broken layouts, corrupted fonts, or ruined files.
* **Transparency & Control:** Clear, predictable output so you always know what was optimized and how much space you saved.
* **Intuitive CLI:** Logical, human-friendly flags that don't require you to memorize complex legacy options.

### A Note on the `extreme` Profile

Don't let the name intimidate you! While **`extreme`** sounds like it might crush your document's quality, it was carefully engineered to be surprisingly usable and well worth testing.

When testing existing tools on aggressive settings (such as Ghostscript's `/screen` preset or heavy compression modes in other utilities), image resolution often severely degrades, turning diagrams and photos into pixelated, unreadable mush. 

`pdftamp` takes a much smarter approach. Even under its `extreme` profile, it aggressively slashes file size on real-world PDF books and media-heavy documents while preserving image clarity and avoiding pixelation as much as possible. Give it a try on your heavy PDFs!

## What it does and doesn't compress

**Compresses:**
- JPEG images (`DCTDecode`) — re-encoded at a lower quality
- Raw/uncompressed and `FlateDecode`/`LZWDecode` images — converted to JPEG
- Uncompressed ("raw") content streams — deflated

**Leaves untouched:**
- `JPXDecode` (JPEG2000), `CCITTFaxDecode` (fax scans), `JBIG2Decode` images
- Already-`FlateDecode` content streams (already compressed)
- Fonts, page/object structure, embedded files, form fields, JavaScript
- Document metadata (`/Info`, XMP, per-image EXIF/ICC) — unless `--strip-metadata`
- Encrypted PDFs — refused unless `--allow-decrypt`, and even then only
  ones with no real password set (a genuinely password-protected file
  can't be bypassed)

**Optional external tools** (auto-detected, not required):
`jpegoptim`, `oxipng`, `pngquant` improve image results when installed;
`qpdf` is required for `--allow-decrypt` and as a repair fallback for
malformed PDFs. Without them, pdftamp falls back to its own pure-Rust
encoders for everything except decryption.

## Installation

### Pre-built packages (recommended)

Download the latest release for your distribution from the
[Releases](https://github.com/AbuKaram01/pdftamp/releases) page.

**Debian / Ubuntu**
```sh
sudo apt install ./pdftamp_0.1.1-1_amd64.deb
```

**Fedora / RHEL / openSUSE**
```sh
sudo dnf install ./pdftamp-0.1.1-1.x86_64.rpm
```

> **Note:** `apt` and `dnf` will automatically pull in the optional
> recommended tools (`jpegoptim`, `oxipng`, `pngquant`, `qpdf`) if
> they're available in your repositories. If you install with
> `dpkg -i` or `rpm -i` directly, dependencies won't be resolved
> automatically.

### Build from source

Clone and build locally:
```sh
git clone https://github.com/AbuKaram01/pdftamp.git
cd pdftamp
cargo build --release
# binary at target/release/pdftamp
```

## Usage

```sh
pdftamp compress input.pdf [output.pdf] [OPTIONS]
pdftamp compress-dir input_dir [output_dir] [OPTIONS]
pdftamp analyze input.pdf [--allow-decrypt]
pdftamp profiles
```

| Option              | Meaning                                                      |
|---------------------|----------------------------------------------------------------|
| `-p, --profile`     | `extreme`/`archive`/`email`/`ebook`/`balanced`/`office`/`print`/`lossless` (default `balanced`) |
| `-q, --quality`     | JPEG quality override, 1-95 (ignored by `lossless`)          |
| `-s, --strip-metadata` | Also strip document/image metadata. **Off by default**    |
| `--allow-decrypt`   | Allow decrypting a PDF with no real password set. **Off by default** |
| `-v, --verbose`     | Print one line per modified object                           |
| `--if-exists`       | `refuse` (default) / `overwrite` / `rename` on a naming collision |
| `--log-file`        | Append a plain-text record of the run to this file            |
| `-n, --dry-run`     | Preview what would happen — nothing is created, overwritten, or renamed |

`compress-dir` mirrors `input_dir`'s structure into `output_dir` if
given, otherwise saves each file next to its own original.

### `--dry-run`

`-n`/`--dry-run` runs the full pipeline — loading the PDF, re-encoding
every eligible image, deflating raw streams, and (with
`--strip-metadata`) simulating metadata removal — entirely in memory,
and prints exactly the numbers a real run would. The only thing it
skips is the final write: nothing is created, overwritten, or renamed
at the output path, its parent directories, or (for `compress-dir`
with an explicit output directory) anywhere under the mirrored tree.

It's honest about failure too: with `--if-exists=refuse`, a dry run
against a path that's already taken reports the same "already exists"
error a real run would — it doesn't pretend the run would have
succeeded. Works with both `compress` and `compress-dir`, and
combines with every other option (`--profile`, `--if-exists`,
`--log-file`, `--verbose`, ...) exactly as it would on a real run.

```sh
pdftamp compress report.pdf --dry-run
pdftamp compress-dir ./scans --if-exists=rename --dry-run
```

### Profiles

| Profile    | Quality | For                                      |
|------------|---------|-------------------------------------------|
| `extreme`  | 25      | Quick previews, bulk low-priority archiving |
| `archive`  | 50      | Documents rarely opened again              |
| `email`    | 65      | Small enough to email or message           |
| `ebook`    | 72      | Reading on a phone/tablet screen           |
| `balanced` | 80      | Sensible default (used unless overridden)  |
| `office`   | 85      | Sharper text/images for work reports       |
| `print`    | 90      | Document will be printed                   |
| `lossless` | —       | Zero quality loss (contracts, legal)       |

## License

AGPL-3.0-or-later — see [LICENSE](LICENSE).
