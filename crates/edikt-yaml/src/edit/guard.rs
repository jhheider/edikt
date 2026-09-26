//! The value guard around every edit primitive (jhheider/edikt#111).
//!
//! A byte splice that picks the wrong range can delete or rewrite values the
//! edit never named, and the result still parses, so nothing downstream
//! notices. After each primitive the guard compares the stream's values with
//! what the evaluator says the edit means: the edited document must equal
//! the in-memory `path = value` / `del(path)` / `path += items` over its old
//! value, and every other document must be unchanged. A mismatch undoes the
//! edit and errors, so a wrong splice fails loudly instead of losing data.
//!
//! The check costs one value projection and one evaluation per primitive,
//! the same order as the full recompose each splice already does.
//!
//! Skipped for a document that uses aliases: there an edit to an anchored
//! node rightly changes every alias of it (and a merge key's view), which the
//! evaluator's plain-tree model can't predict.

use edikt_core::{EditError, Expr, Step, Value, eval, render_path};

use super::Strictness;
use crate::Yaml;
use crate::compose::{Node, NodeKind};

/// What the evaluator says a primitive does to its document's value.
pub(crate) enum Intent<'a> {
    Set(&'a Value),
    Delete,
    Append(&'a [Value]),
}

impl Intent<'_> {
    /// The document value after the edit, per the evaluator, or `None` when
    /// the evaluator can't say (it errors where the format has a reading).
    fn expected(&self, path: &[Step], old: &Value) -> Option<Value> {
        let target = Box::new(Expr::Path(path.to_vec()));
        let expr = match self {
            Intent::Set(v) => Expr::Assign(target, Box::new(Expr::Literal((*v).clone()))),
            Intent::Delete => Expr::Call("del".into(), vec![Expr::Path(path.to_vec())]),
            Intent::Append(items) => Expr::AddAssign(
                target,
                Box::new(Expr::Literal(Value::Array(items.to_vec()))),
            ),
        };
        eval(&expr, old).ok()?.into_iter().next()
    }
}

impl Yaml {
    /// Run `op`, a primitive editing document `idx` at `path`, and undo it
    /// with an error if the stream's values don't come out as `intent` says.
    /// A lenient (mapped) edit may be a no-op where the path misses; a
    /// strict one that changes nothing must also have been meant to.
    pub(crate) fn guarded(
        &mut self,
        idx: usize,
        path: &[Step],
        intent: Intent<'_>,
        strict: Strictness,
        op: impl FnOnce(&mut Self) -> Result<(), EditError>,
    ) -> Result<(), EditError> {
        if self.docs.iter().any(|d| has_alias(&self.source, d)) {
            return op(self);
        }
        let before = self.source.clone();
        let old: Vec<Value> = (0..self.docs.len()).map(|i| self.doc_value(i)).collect();
        op(self)?;
        if self.source == before && strict == Strictness::Lenient {
            return Ok(());
        }
        let Some(want) = intent.expected(path, &old[idx]) else {
            return Ok(());
        };
        let same_shape = self.docs.len() == old.len();
        let ok = same_shape
            && (0..old.len()).all(|i| {
                let got = self.doc_value(i);
                if i == idx { got == want } else { got == old[i] }
            });
        if ok {
            return Ok(());
        }
        self.restore(before)?;
        Err(EditError::new(format!(
            "cannot edit {} in place: the splice would change other values too \
             (a bug, please report it)",
            render_path(path)
        )))
    }
}

/// Does `node` hold an alias (`*name`) anywhere? Only an alias's text starts
/// with `*`: a plain scalar can't, and quoted and block ones start with their
/// indicator.
fn has_alias(source: &str, node: &Node) -> bool {
    match &node.kind {
        NodeKind::Scalar(_) => source[node.span.clone()].starts_with('*'),
        NodeKind::Sequence(items) => items.iter().any(|n| has_alias(source, n)),
        NodeKind::Mapping(entries) => entries.iter().any(|e| has_alias(source, &e.value)),
    }
}
