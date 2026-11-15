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
| `io` | always | disc images to read (`DiscFile`), a tree of files to read and write (`Vfs`: a folder, a pack file, a game folder read only), the progress log (`Ctx`) |
| `psx` | `psx` (default) | PlayStation media in place of ffmpeg: XA ADPCM sound and STR (MDEC) movies from raw CD sectors, out as FLAC and Motion-JPEG AVI |
| `np` | `np` (default) | the converters' small float64 matrix maths (`svd`, `inv4`, `fma`), the same bits on every platform |

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
