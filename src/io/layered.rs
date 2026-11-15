//! Layers: a `Vfs` written on top of others read through (a pack's install: the extras over the Tekken 3 pack).

use super::{Entry, Vfs};
use anyhow::{Result, bail};

/// `top` over `below` (first over the rest): a file is read from the first layer that has it; everything is written
/// to `top`, which so holds every file a writer changed or added, the layers below as they were.
pub struct Layered<'a> {
    pub top: &'a dyn Vfs,
    pub below: Vec<&'a dyn Vfs>,
}

impl Layered<'_> {
    fn layers(&self) -> impl Iterator<Item = &dyn Vfs> {
        std::iter::once(self.top).chain(self.below.iter().copied())
    }
}

impl Vfs for Layered<'_> {
    fn read(&self, path: &str) -> Result<Vec<u8>> {
        match self.layers().find(|l| l.exists(path)) {
            Some(l) => l.read(path),
            None => self.top.read(path),
        }
    }
    fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        self.top.write(path, data)
    }
    fn exists(&self, path: &str) -> bool {
        self.layers().any(|l| l.exists(path))
    }
    /// Every layer's entries, each name once (a folder where any layer has one).
    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let mut out: Vec<Entry> = Vec::new();
        for l in self.layers() {
            for e in l.list(dir)? {
                match out.iter_mut().find(|o| o.name == e.name) {
                    Some(o) => o.is_dir |= e.is_dir,
                    None => out.push(e),
                }
            }
        }
        out.sort();
        Ok(out)
    }
    /// From `top`; a path a layer below has cannot go (they are read only).
    fn remove_all(&self, path: &str) -> Result<()> {
        if self.below.iter().any(|l| l.exists(path)) {
            bail!("{path} is in a layer below, read only: it cannot be removed");
        }
        self.top.remove_all(path)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::io::StdFs;

    #[test]
    fn reads_through_writes_on_top() {
        let dir = std::env::temp_dir().join(format!("ok_layered_{}", std::process::id()));
        let (base, top) = (StdFs(dir.join("base")), StdFs(dir.join("top")));
        base.write("a/x.json", b"base x").unwrap();
        base.write("a/y.json", b"base y").unwrap();
        let view = Layered { top: &top, below: vec![&base] };
        view.write("a/y.json", b"top y").unwrap();
        view.write("b/z.json", b"top z").unwrap();
        assert_eq!(view.read("a/x.json").unwrap(), b"base x");
        assert_eq!(view.read("a/y.json").unwrap(), b"top y");
        assert_eq!(base.read("a/y.json").unwrap(), b"base y");
        let names: Vec<String> = view.list("").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(view.list("a").unwrap().len(), 2);
        assert!(view.remove_all("a/x.json").is_err());
        view.remove_all("b").unwrap();
        assert!(!view.exists("b/z.json"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
