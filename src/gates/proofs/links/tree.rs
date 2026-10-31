//! The tree a `code:` link resolves against: each production source and the items it defines.

use std::collections::BTreeMap;

use super::{Defined, Link, RUST, Resolved, Source, defined};
use crate::gates::metrics::prodlines;
use crate::run::Ctx;

/// Every production source and its items, with each item found by its last name.
pub(super) struct Tree {
    sources: Vec<Source>,
    named: BTreeMap<String, Vec<(usize, usize)>>,
}

impl Tree {
    pub(super) fn read(ctx: &Ctx) -> Result<Self, String> {
        let read = prodlines::for_each_source(ctx, &|src| Ok(defined(src)))?;
        let source = |(file, items): (String, Vec<Defined>)| Source {
            module: module(&file),
            file,
            items,
        };
        let sources: Vec<Source> = read.into_iter().map(source).collect();
        let mut named: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
        for (s, source) in sources.iter().enumerate() {
            for (i, item) in source.items.iter().enumerate() {
                if let Some(last) = item.path.last() {
                    named.entry(last.clone()).or_default().push((s, i));
                }
            }
        }
        Ok(Self { sources, named })
    }

    /// With a file, the item must be in a source whose path ends in it. The copies of one item in
    /// one file, kept apart by `#[cfg]`, are one item. The link's own `model` is no source.
    pub(super) fn resolve(&self, link: &Link, model: &str) -> Resolved<'_> {
        if link.outside() {
            let why = "its file is outside this project, whose code alone the gate reads; link code \
                       here, or start the comment with `none;` and say why";
            return Resolved::Missing(why.to_string());
        }
        let files = link.file.as_deref().map(|file| self.ending(file, model));
        if let (Some(file), Some(files)) = (&link.file, &files) {
            match (files.len(), link.path.is_empty()) {
                (0, _) => return Resolved::Missing(format!("no source file ends in `{file}`")),
                (1, true) => return Resolved::File,
                (_, true) => {
                    return Resolved::Many(files.iter().map(ToString::to_string).collect());
                }
                _ => {}
            }
        }
        let hits = self.hits(&link.path, files.as_deref(), model);
        let Some(&(source, item)) = hits.first() else {
            let first = link.path.first().map_or("", String::as_str);
            let path = link.path.join("::");
            return match &link.file {
                None if RUST.contains(&first) => Resolved::Rust,
                None => Resolved::Missing(format!("no item's path ends in `{path}`")),
                Some(file) => {
                    Resolved::Missing(format!("no file that ends in `{file}` holds `{path}`"))
                }
            };
        };
        let copy = |(s, i): &(&Source, &Defined)| s.file == source.file && i.path == item.path;
        if hits.iter().all(copy) {
            return Resolved::Item(source, hits.iter().map(|(_, item)| *item).collect());
        }
        let place = |(s, i): &(&Source, &Defined)| format!("{}:{}", s.file, i.line);
        Resolved::Many(hits.iter().map(place).collect())
    }

    /// The sources other than `model` whose path is `file` or ends in `/file`.
    fn ending(&self, file: &str, model: &str) -> Vec<&str> {
        let tail = format!("/{file}");
        let ends = |source: &&Source| source.file == file || source.file.ends_with(&tail);
        self.sources
            .iter()
            .filter(|source| source.file != model)
            .filter(ends)
            .map(|source| source.file.as_str())
            .collect()
    }

    /// The items outside `model`, in `files` when it is given, whose module path and own path,
    /// read as one, end in `path`.
    fn hits(
        &self,
        path: &[String],
        files: Option<&[&str]>,
        model: &str,
    ) -> Vec<(&Source, &Defined)> {
        let last = path.last().map_or("", String::as_str);
        let places = self.named.get(last).into_iter().flatten();
        let items = places.map(|(s, i)| (&self.sources[*s], &self.sources[*s].items[*i]));
        let held = |(source, _): &(&Source, &Defined)| {
            source.file != model && files.is_none_or(|files| files.contains(&source.file.as_str()))
        };
        let ends =
            |(source, item): &(&Source, &Defined)| ends_with(&source.module, &item.path, path);
        items.filter(held).filter(ends).collect()
    }
}

/// Whether the module path and the item path, read as one, end in `wanted`.
fn ends_with(module: &[String], path: &[String], wanted: &[String]) -> bool {
    let len = module.len() + path.len();
    let full = module.iter().chain(path);
    len >= wanted.len() && full.skip(len - wanted.len()).eq(wanted)
}

/// The module path a file's place gives its items: the crate's directory with `-` read as `_`,
/// then each directory under `src` and the file's name, which `lib`, `main` and `mod` leave out.
pub(super) fn module(file: &str) -> Vec<String> {
    let parts: Vec<&str> = file.trim_end_matches(".rs").split('/').collect();
    let (crate_dir, inner) = match parts.iter().rposition(|part| *part == "src") {
        Some(at) => (at.checked_sub(1).map(|it| parts[it]), &parts[at + 1..]),
        None => (None, &parts[..]),
    };
    let named = inner
        .iter()
        .filter(|part| !matches!(**part, "lib" | "main" | "mod"));
    let all = crate_dir.into_iter().chain(named.copied());
    all.map(|part| part.replace('-', "_")).collect()
}
