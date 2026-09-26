//! Deleting from the span tree: one mapping entry or sequence item, or a
//! container emptied by a trailing `del(...[])`.

use super::extent::{block_end, trim_newline};
use super::resolve::{Resolved, resolve};
use crate::Yaml;
use crate::compose::{Node, NodeKind};
use crate::layout::line_start;
use edikt_core::{EditError, Step};

impl Yaml {
    /// Delete the mapping entry or sequence item at `path` in document `idx`.
    pub(crate) fn delete(&mut self, idx: usize, path: &[Step]) -> Result<(), EditError> {
        // Fan-out delete: resolve the iterate to concrete index/key paths and
        // splice each through the ordinary single-target machinery, back-to-
        // front so indices stay valid as the collection shrinks.
        if path.contains(&Step::Iterate) {
            // A **trailing** iterate (`del(.a[])`) empties the container it
            // names: rewrite the whole entry/container to its inline empty
            // spelling (`a: []` / `a: {}`), the jq-analogue of leaving `[]`.
            // Block-form items and the comments inside the emptied region go
            // with it. A nested iterate (`del(.a[].b)`) composes the per-item
            // deletes below instead, matching YAML's block semantics.
            if let Some(Step::Iterate) = path.last() {
                return self.delete_within(idx, &path[..path.len() - 1]);
            }
            let whole = self.doc_value(idx);
            let paths = edikt_core::expand_delete_paths(path, &whole)?;
            for p in &paths {
                self.delete(idx, p)?;
            }
            return Ok(());
        }
        let Some((last, parent_path)) = path.split_last() else {
            return Err(EditError::new("cannot delete the whole document"));
        };
        // jq semantics (and the other formats): deleting a missing key, an
        // out-of-range index, or through an absent parent is a **no-op**.
        let Resolved::Found(parent) = resolve(self.root(idx), parent_path) else {
            return Ok(());
        };
        let range = match (last, &parent.kind) {
            (Step::Field(k), NodeKind::Mapping(entries)) => {
                let Some(entry) = entries.iter().find(|e| &e.key == k) else {
                    return Ok(());
                };
                line_start(&self.source, entry.key_span.start)
                    ..block_end(&self.source, &entry.value)
            }
            (Step::Index(i), NodeKind::Sequence(items)) => {
                let Some(idx) = edikt_core::resolve_index(*i, items.len()) else {
                    return Ok(());
                };
                let item = &items[idx];
                line_start(&self.source, item.span.start)..block_end(&self.source, item)
            }
            _ => return Ok(()),
        };
        self.commit(range, "")
    }

    /// The container emptied by a trailing `del(...[])`: rewrite the entry (or
    /// the root document) to its inline empty spelling, like jq leaves `[]`.
    /// A missing parent or non-container is a no-op, matching delete.
    fn delete_within(&mut self, idx: usize, prefix: &[Step]) -> Result<(), EditError> {
        let empty = |kind: &NodeKind| -> &'static str {
            match kind {
                NodeKind::Sequence(_) => "[]",
                _ => "{}",
            }
        };
        // block_end consumes the trailing newline; the empty rewrite keeps it
        // (all of a CRLF, not just its `\n`).
        let end_of = |n: &Node| trim_newline(&self.source, block_end(&self.source, n));
        match prefix.split_last() {
            // Root container (`del(.[])`): replace the whole document's bytes.
            None => {
                let Resolved::Found(root) = resolve(self.root(idx), &[]) else {
                    return Ok(());
                };
                let range = line_start(&self.source, root.span.start)..end_of(root);
                self.commit(range, empty(&root.kind))
            }
            Some((Step::Field(k), rest)) => {
                let Resolved::Found(map) = resolve(self.root(idx), rest) else {
                    return Ok(());
                };
                let NodeKind::Mapping(entries) = &map.kind else {
                    return Ok(());
                };
                let Some(entry) = entries.iter().find(|e| &e.key == k) else {
                    return Ok(());
                };
                let key = &self.source[entry.key_span.start..entry.key_span.end];
                let range = entry.key_span.start..end_of(&entry.value);
                let text = format!("{key}: {}", empty(&entry.value.kind));
                self.commit(range, &text)
            }
            _ => Ok(()),
        }
    }
}
