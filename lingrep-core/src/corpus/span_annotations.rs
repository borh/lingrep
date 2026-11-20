use crate::token_match::SentenceSpan;

pub(crate) fn partition<'a>(
    kind: &str,
    boundaries: impl Iterator<Item = Option<&'a str>>,
) -> Vec<SentenceSpan> {
    let mut spans: Vec<SentenceSpan> = Vec::new();
    for (index, label) in boundaries.enumerate() {
        match label {
            Some("B") => spans.push(SentenceSpan {
                kind: kind.to_owned(),
                start: index,
                end: index + 1,
                label: None,
                head: None,
                governor: None,
            }),
            Some("I") => {
                let Some(span) = spans.last_mut() else {
                    return Vec::new();
                };
                span.end = index + 1;
            }
            _ => return Vec::new(),
        }
    }
    spans
}

pub(crate) fn bunsetsu_heads<'a>(
    spans: &mut [SentenceSpan],
    annotation: impl Fn(usize) -> (bool, Option<&'a str>, Option<usize>),
) {
    for index in 0..spans.len() {
        let range = spans[index].start..spans[index].end;
        let mut marked = None;
        let mut marked_count = 0;
        let mut exiting = None;
        let mut exits = 0;
        for word in range.clone() {
            let (is_head, label, ud_head) = annotation(word);
            if is_head {
                marked_count += 1;
                marked = Some((word, label));
            }
            if ud_head.is_some_and(|head| head == 0 || !range.contains(&(head - 1))) {
                exiting = Some(word);
                exits += 1;
            }
        }
        let marked = marked.filter(|_| marked_count == 1);
        let head = marked
            .map(|(word, _)| word)
            .or(exiting.filter(|_| exits == 1));
        spans[index].head = head;
        spans[index].label = marked
            .and_then(|(_, label)| label)
            .filter(|label| !label.is_empty())
            .map(str::to_owned);
        if let Some(mut word) = head {
            for _ in range.clone() {
                let Some(target) = annotation(word).2.and_then(|id| id.checked_sub(1)) else {
                    break;
                };
                if !range.contains(&target) {
                    let parent = spans.partition_point(|s| s.end <= target);
                    if parent < spans.len() {
                        spans[index].governor = Some(parent);
                    }
                    break;
                }
                word = target;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_layer_and_orphan_continuation_have_no_partition() {
        for labels in [[Some("B"), None], [Some("I"), Some("B")]] {
            assert!(partition("bunsetsu", labels.into_iter()).is_empty());
        }
    }

    #[test]
    fn semantic_head_can_reach_the_governor_through_a_function_word() {
        let mut spans = partition("bunsetsu", ["B", "I", "B"].into_iter().map(Some));
        let rows = [
            (true, Some("NP"), Some(2)),
            (false, None, Some(3)),
            (true, Some("VP"), Some(0)),
        ];
        bunsetsu_heads(&mut spans, |word| rows[word]);
        assert_eq!(
            spans,
            [
                SentenceSpan {
                    kind: "bunsetsu".into(),
                    start: 0,
                    end: 2,
                    head: Some(0),
                    label: Some("NP".into()),
                    governor: Some(1)
                },
                SentenceSpan {
                    kind: "bunsetsu".into(),
                    start: 2,
                    end: 3,
                    head: Some(2),
                    label: Some("VP".into()),
                    governor: None
                },
            ]
        );
    }
}
