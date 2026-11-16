# ok_converter_common

What the OpenKumite converters share, with no game in it. OpenKumite (an open-source 3D fighting game engine)
converts your own copies of the games in the game itself, through these crates:

| Repository | Converts |
|---|---|
| ok_tekken3_converter | your Tekken 3 disc: the whole game (required) |
| ok_tekken2_converter | your Tekken 2 disc: its fighters, as a pack beside Tekken 3's |
| ok_hl_cs_converter | your Counter-Strike game: its levels as arenas, as a pack beside Tekken 3's |

This crate holds no game data and knows no game.

## Modules

| Module | Feature | What it is |
|---|---|---|
| `disc` | `disc` | disc images in the formats players keep them in, each read as the disc's tracks of raw 2352-byte sectors (see below), and the data track's files (ISO 9660) |
| `io` | always | disc images to read (`DiscFile`), a tree of files to read and write (`Vfs`: a folder, a pack file, a game folder read only), the progress log (`Ctx`) |
| `psx` | `psx` (default) | PlayStation media in place of ffmpeg: XA ADPCM sound and STR (MDEC) movies from raw CD sectors, out as FLAC and Motion-JPEG AVI |
| `np` | `np` (default) | the converters' small float64 matrix maths (`svd`, `inv4`, `fma`), the same bits on every platform |

### Disc images (`disc`)

`Disc::open` (or `Disc::open_path`) takes the image the player chose, with the files beside it:

| Format | Files | Notes |
|---|---|---|
| CUE/BIN | `.cue` with its `.bin` files (one per track or one for all; `.wav` too), or a `.bin` alone | a `.cue` beside the `.bin` is found; names match loosely (`_` for space) |
| ISO | `.iso` | 2048 bytes of data per sector only: no XA audio, no movies, no CD audio (`DiscFile::whole_sectors` is false) |
| CHD | `.chd` | MAME's compressed CD image (cdlz, cdzl, cdfl, cdzs), read in pure Rust |
| CloneCD | `.ccd` with its `.img` | |
| Alcohol 120% | `.mds` with its `.mdf` | version 1 |
| ECM | `.bin.ecm`, alone or named by a `.cue` | the sectors' error codes rebuilt |
| PBP | a PSP `EBOOT.PBP` made by popstation | not an encrypted one from a store; multi-disc: the first disc |

Each `Track` reads as the split `.bin` of that track would: an audio track from its pregap (silence where the image
keeps none), a data track from INDEX 01, sectors rebuilt whole (sync, header, EDC and ECC) where the image keeps less.
`main_image` picks the image among several files (a folder, a multi-file pick). The tests write a small disc in every
format and read it back; `real_disc_formats` (ignored) does it with a real disc (`OK_DISC_CUE`, `OK_DISC_OUT`; with
`chdman` on the PATH it makes the `.chd` too).

A converter that needs only files (Counter-Strike) takes it with `default-features = false`.

`serve` (`cargo run --release --bin serve -- [PORT] [DIR]`) is the local web server of the converters' viewers.

## Checks

`sh check.sh` runs formatting, clippy, the tests and `cargo deny`; the git hooks (cargo-husky) and CI run it.

## Releases

A tag `vX.Y.Z` (Cargo.toml's version; only organisation admins push `v*` tags) publishes a release: `serve` for
Linux (amd64, arm64), Windows and macOS (universal), with SHA256SUMS (`.github/workflows/release.yml`). `main` takes
pull requests only, its CI passing; Dependabot opens one grouped pull request a week.

## License

MIT ([LICENSE](LICENSE)).
