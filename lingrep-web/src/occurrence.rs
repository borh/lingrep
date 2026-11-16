//! The words an occurrence binds, which identify it across a frequency line
//! and a question run over the same analysis.

use std::ops::Range;

use lingrep_core::corpus::conllu::EnhancedNodeId;
use lingrep_core::fsm::GraphBinding;
use serde::{Deserialize, Serialize};

/// A named capture's span, a graph node's word, or an edge endpoint's word,
/// named `edge.head` or `edge.dependent`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Role {
    pub name: String,
    pub start: usize,
    pub end: usize,
}

/// The sorted, deduplicated roles of one match; graph nodes other than words
/// have no token position and are left out.
pub fn roles<'a>(
    captures: impl IntoIterator<Item = (&'a str, Range<usize>)>,
    graph_bindings: &[GraphBinding],
) -> Vec<Role> {
    let mut roles = captures
        .into_iter()
        .map(|(name, span)| Role {
            name: name.to_owned(),
            start: span.start,
            end: span.end,
        })
        .collect::<Vec<_>>();
    let mut word = |name: String, node: &EnhancedNodeId| {
        if let EnhancedNodeId::Word { token_index } = *node {
            roles.push(Role {
                name,
                start: token_index,
                end: token_index + 1,
            });
        }
    };
    for binding in graph_bindings {
        match binding {
            GraphBinding::Node { name, node, .. } => word(name.to_string(), node),
            GraphBinding::Edge {
                name,
                head,
                dependent,
                ..
            } => {
                word(format!("{name}.head"), head);
                word(format!("{name}.dependent"), dependent);
            }
        }
    }
    roles.sort();
    roles.dedup();
    roles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_name_captures_graph_words_and_edge_endpoints_once_each() {
        let graph = [
            GraphBinding::Node {
                name: "v".into(),
                node: EnhancedNodeId::Word { token_index: 9 },
                attributes: Box::default(),
            },
            GraphBinding::Edge {
                name: "e".into(),
                head: EnhancedNodeId::Word { token_index: 9 },
                dependent: EnhancedNodeId::Word { token_index: 2 },
                relation: "obj".into(),
            },
            GraphBinding::Node {
                name: "x".into(),
                node: EnhancedNodeId::Empty {
                    boundary: 3,
                    subindex: 1,
                },
                attributes: Box::default(),
            },
        ];
        let role = |name: &str, start, end| Role {
            name: name.into(),
            start,
            end,
        };
        assert_eq!(
            roles([("p", 3..4), ("n", 2..3), ("n", 2..3)], &graph),
            [
                role("e.dependent", 2, 3),
                role("e.head", 9, 10),
                role("n", 2, 3),
                role("p", 3, 4),
                role("v", 9, 10),
            ]
        );
    }
}
