//! What the OpenKumite converters share, with no game in it:
//!
//! - [`disc`] (feature `disc`): disc images in the formats players keep them in (.cue/.bin, .iso, .chd, .ccd/.img,
//!   .mds/.mdf, .ecm, .pbp; or in an archive: .zip, .7z, .tar, .gz, .xz, .zst), read as tracks of raw sectors, and
//!   the data track's ISO 9660 files;
//! - [`io`]: disc images to read, a tree of files to read and write (a folder, a pack file, a game folder read only),
//!   the progress log;
//! - [`psx`] (feature `psx`): PlayStation media in place of ffmpeg: XA ADPCM sound and STR (MDEC) movies from raw CD
//!   sectors, out as FLAC and Motion-JPEG AVI;
//! - [`np`] (feature `np`): the converters' small float64 matrix maths with the same bits on every platform.
//!
//! The converters (ok_tekken3_converter, ok_tekken2_converter, ok_hl_cs_converter) depend on it, and through them
//! OpenKumite, which converts in the game.

#[cfg(feature = "disc")]
pub mod disc;
pub mod io;
#[cfg(feature = "np")]
pub mod np;
#[cfg(feature = "psx")]
pub mod psx;
