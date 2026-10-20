use crate::fsm::types::Match;

pub fn canonicalize_matches(matches: &mut [Match]) {
    matches.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.pattern.cmp(&right.pattern))
            .then_with(|| left.captures.cmp(&right.captures))
            .then_with(|| left.graph_bindings.cmp(&right.graph_bindings))
            .then_with(|| left.lexical.cmp(&right.lexical))
    });
}

pub fn canonical_matches(mut matches: Vec<Match>) -> Vec<Match> {
    canonicalize_matches(&mut matches);
    matches
}
