//! The uniform comment model for comment-preserving conversion.
//!
//! Comments cross formats through a shared vocabulary of three kinds — **head**
//! (own-line comments before a node), **inline** (a trailing comment on the
//! node's line), and **foot** (own-line comments after a node that no following
//! sibling claims — in practice, trailing comments at the end of a container or
//! document). Each format parses its comments *out* to this model
//! ([`crate::Document::to_commented`]), and each format's emitter decides per
//! kind: place it, remap it to a kind it supports (warn), or drop it (warn) —
//! N-in + N-out against one model, not N×N per format pair.
//!
//! Comment text is stored without delimiters (`# `, `// `, `; `) and trimmed,
//! so the target format re-delimits it natively. A multi-line block comment
//! contributes one `head`/`foot` entry per line.

use crate::{Step, Value};

/// The comments attached to one node, by kind.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Comments {
    /// Own-line comments immediately before the node, one entry per line.
    pub head: Vec<String>,
    /// A trailing comment on the node's own line.
    pub inline: Option<String>,
    /// Own-line comments after the node that no following sibling claims.
    pub foot: Vec<String>,
}

impl Comments {
    pub fn is_empty(&self) -> bool {
        self.head.is_empty() && self.inline.is_none() && self.foot.is_empty()
    }
}

/// A [`Value`] enriched with per-node comments — what conversion carries so a
/// commented source survives `-T` into a commented target.
#[derive(Debug, Clone, PartialEq)]
pub struct Commented {
    pub comments: Comments,
    pub node: CommentedNode,
}

/// The shape of a [`Commented`] node, mirroring [`Value`].
#[derive(Debug, Clone, PartialEq)]
pub enum CommentedNode {
    /// A scalar (never `Value::Array`/`Value::Object` — those are the variants
    /// below, so comments can attach to every element/entry).
    Scalar(Value),
    Array(Vec<Commented>),
    Object(Vec<(String, Commented)>),
}

impl Commented {
    /// Wrap a plain value with no comments anywhere.
    pub fn from_value(value: &Value) -> Commented {
        let node = match value {
            Value::Array(items) => {
                CommentedNode::Array(items.iter().map(Commented::from_value).collect())
            }
            Value::Object(entries) => CommentedNode::Object(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone(), Commented::from_value(v)))
                    .collect(),
            ),
            scalar => CommentedNode::Scalar(scalar.clone()),
        };
        Commented {
            comments: Comments::default(),
            node,
        }
    }

    /// A scalar node with no comments (convenience for extractors).
    pub fn scalar(value: Value) -> Commented {
        Commented {
            comments: Comments::default(),
            node: CommentedNode::Scalar(value),
        }
    }

    /// Strip comments back to the plain value model.
    pub fn to_value(&self) -> Value {
        match &self.node {
            CommentedNode::Scalar(v) => v.clone(),
            CommentedNode::Array(items) => {
                Value::Array(items.iter().map(Commented::to_value).collect())
            }
            CommentedNode::Object(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_value()))
                    .collect(),
            ),
        }
    }

    /// Does this node or any descendant carry a comment?
    pub fn has_comments(&self) -> bool {
        if !self.comments.is_empty() {
            return true;
        }
        match &self.node {
            CommentedNode::Scalar(_) => false,
            CommentedNode::Array(items) => items.iter().any(Commented::has_comments),
            CommentedNode::Object(entries) => entries.iter().any(|(_, v)| v.has_comments()),
        }
    }

    /// Attach trailing document comments as the foot of the deepest last entry
    /// — the node they physically follow, so re-emission keeps them at the end.
    pub fn attach_trailing_foot(&mut self, lines: Vec<String>) {
        match &mut self.node {
            CommentedNode::Object(entries) if !entries.is_empty() => {
                entries.last_mut().unwrap().1.attach_trailing_foot(lines);
            }
            CommentedNode::Array(items) if !items.is_empty() => {
                items.last_mut().unwrap().attach_trailing_foot(lines);
            }
            _ => self.comments.foot.extend(lines),
        }
    }

    /// The nodes a pure path selects, in document order — mirroring the
    /// evaluator's path semantics (a missing field/index yields nothing;
    /// `[]` iterates elements/values), so the results align 1:1 with
    /// [`crate::eval`] on the same path. A step that cannot apply (e.g. a field
    /// of a scalar) yields nothing here; the evaluator errors first in that
    /// case, so the mismatch is never observed.
    pub fn descend(&self, path: &[Step]) -> Vec<&Commented> {
        let mut stream = vec![self];
        for step in path {
            let mut next = Vec::new();
            for node in stream {
                match (step, &node.node) {
                    (Step::Field(k), CommentedNode::Object(entries)) => {
                        next.extend(entries.iter().find(|(kk, _)| kk == k).map(|(_, v)| v));
                    }
                    (Step::Index(i), CommentedNode::Array(items)) => {
                        let idx = if *i < 0 { items.len() as i64 + i } else { *i };
                        if idx >= 0 && (idx as usize) < items.len() {
                            next.push(&items[idx as usize]);
                        }
                    }
                    (Step::Iterate, CommentedNode::Array(items)) => next.extend(items.iter()),
                    (Step::Iterate, CommentedNode::Object(entries)) => {
                        next.extend(entries.iter().map(|(_, v)| v));
                    }
                    _ => {}
                }
            }
            stream = next;
        }
        stream
    }
}

/// One flattened `key = value` line with the comments it carries — the shape
/// the flat emitters (INI sections, `.env`) place comments through.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlatEntry {
    pub key: String,
    pub value: String,
    pub comments: Comments,
}

/// Flatten a commented tree to dotted-key entries (the commented analogue of
/// [`crate::convert::flatten`]). A container's own comments ride along: its
/// `head` (and `inline`, which has no line of its own once flattened) prepend
/// to its first entry's `head`; its `foot` appends to its last entry's `foot`.
/// An empty container vanishes, its comments carried to... nowhere — the caller
/// sees them dropped via [`Commented::has_comments`] on the re-projected result;
/// in practice empty containers with comments are vanishingly rare.
pub fn flatten_commented(node: &Commented) -> Vec<FlatEntry> {
    let mut out = Vec::new();
    walk("", node, &mut out);
    out
}

fn walk(prefix: &str, node: &Commented, out: &mut Vec<FlatEntry>) {
    match &node.node {
        CommentedNode::Object(entries) => {
            let first = out.len();
            for (k, v) in entries {
                walk(&join_key(prefix, k), v, out);
            }
            distribute_container_comments(node, first, out);
        }
        CommentedNode::Array(items) => {
            let first = out.len();
            for (i, v) in items.iter().enumerate() {
                walk(&join_key(prefix, &i.to_string()), v, out);
            }
            distribute_container_comments(node, first, out);
        }
        CommentedNode::Scalar(v) => out.push(FlatEntry {
            key: prefix.to_string(),
            value: v.to_raw_string(),
            comments: node.comments.clone(),
        }),
    }
}

/// Attach a flattened container's own comments to its first/last entries.
fn distribute_container_comments(node: &Commented, first: usize, out: &mut [FlatEntry]) {
    if node.comments.is_empty() || out.len() <= first {
        return;
    }
    let mut head = node.comments.head.clone();
    // An inline comment loses its own line when the container flattens; it
    // becomes the last head line of the first entry.
    head.extend(node.comments.inline.clone());
    let existing = std::mem::take(&mut out[first].comments.head);
    head.extend(existing);
    out[first].comments.head = head;
    let last = out.len() - 1;
    out[last].comments.foot.extend(node.comments.foot.clone());
}

fn join_key(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commented(head: &[&str], inline: Option<&str>, node: CommentedNode) -> Commented {
        Commented {
            comments: Comments {
                head: head.iter().map(|s| s.to_string()).collect(),
                inline: inline.map(|s| s.to_string()),
                foot: Vec::new(),
            },
            node,
        }
    }

    #[test]
    fn from_value_round_trips_and_is_comment_free() {
        let v = Value::Object(vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Array(vec![Value::Str("x".into())])),
        ]);
        let c = Commented::from_value(&v);
        assert!(!c.has_comments());
        assert_eq!(c.to_value(), v);
    }

    #[test]
    fn descend_mirrors_eval_paths() {
        let tree = Commented {
            comments: Comments::default(),
            node: CommentedNode::Object(vec![
                (
                    "a".into(),
                    commented(&["on a"], None, CommentedNode::Scalar(Value::Int(1))),
                ),
                (
                    "xs".into(),
                    Commented {
                        comments: Comments::default(),
                        node: CommentedNode::Array(vec![
                            Commented::scalar(Value::Int(10)),
                            commented(&[], Some("last"), CommentedNode::Scalar(Value::Int(20))),
                        ]),
                    },
                ),
            ]),
        };
        // identity
        assert_eq!(tree.descend(&[]).len(), 1);
        // field
        let a = tree.descend(&[Step::Field("a".into())]);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].comments.head, vec!["on a"]);
        // negative index
        let last = tree.descend(&[Step::Field("xs".into()), Step::Index(-1)]);
        assert_eq!(last[0].comments.inline.as_deref(), Some("last"));
        // iterate
        assert_eq!(
            tree.descend(&[Step::Field("xs".into()), Step::Iterate])
                .len(),
            2
        );
        // a miss yields nothing, matching the evaluator
        assert!(tree.descend(&[Step::Field("nope".into())]).is_empty());
    }

    #[test]
    fn flatten_carries_comments_to_dotted_keys() {
        let tree = Commented {
            comments: Comments {
                head: vec!["banner".into()],
                inline: None,
                foot: vec!["trailer".into()],
            },
            node: CommentedNode::Object(vec![(
                "a".into(),
                commented(
                    &["section"],
                    None,
                    CommentedNode::Object(vec![(
                        "b".into(),
                        commented(&[], Some("why"), CommentedNode::Scalar(Value::Int(1))),
                    )]),
                ),
            )]),
        };
        let flat = flatten_commented(&tree);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].key, "a.b");
        assert_eq!(flat[0].value, "1");
        // banner (root head) + section (container head) land on the first
        // entry; the scalar keeps its inline; root foot lands on the last.
        assert_eq!(flat[0].comments.head, vec!["banner", "section"]);
        assert_eq!(flat[0].comments.inline.as_deref(), Some("why"));
        assert_eq!(flat[0].comments.foot, vec!["trailer"]);
    }
}
