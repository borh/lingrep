-- Input is one complete ling --json matches --evidence --pattern '^ []+ $.' run.
CREATE TEMP TABLE events AS
SELECT schema, type, data
FROM read_json(getvariable('matches'), format = 'newline_delimited',
               columns = {schema: 'VARCHAR', type: 'VARCHAR', data: 'JSON'});

CREATE TEMP TABLE sentences AS
SELECT (data->>'corpus_sentence_ordinal')::BIGINT AS sentence,
       data->>'source_name' AS source_name,
       (data->>'document_sentence_ordinal')::BIGINT AS document_sentence,
       (data->'span'->>'start')::BIGINT AS first_word,
       (data->'span'->>'end')::BIGINT AS word_count,
       json_transform(data->'evidence', '{
           "words":[{"word":"BIGINT","head":"VARCHAR","rel":"VARCHAR"}],
           "spans":[{"kind":"VARCHAR","start":"BIGINT","end":"BIGINT","governor":"BIGINT"}]
       }') AS evidence
FROM events WHERE type = 'match';

-- Reject partial exports and pattern hits that omit or repeat words or sentences.
CREATE TEMP TABLE input_check AS
SELECT CASE WHEN
    count(*) FILTER (WHERE schema IS DISTINCT FROM 'lingrep.query/v2') = 0
    AND count(*) FILTER (WHERE type = 'run') = 1
    AND count(*) FILTER (WHERE type = 'run' AND (data->>'mode') = 'matches') = 1
    AND count(*) FILTER (WHERE type = 'complete') = 1
    AND count(*) FILTER (WHERE type = 'complete' AND (data->>'coverage') = 'full') = 1
    AND max((data->'counters'->>'sentences_admitted')::BIGINT) = (SELECT count(*) FROM sentences)
    AND max((data->'counters'->>'tokens_scanned')::BIGINT) = (SELECT coalesce(sum(word_count), 0) FROM sentences)
    AND (SELECT count(DISTINCT sentence) = count(*) FROM sentences)
    AND NOT EXISTS (SELECT 1 FROM sentences WHERE first_word IS DISTINCT FROM 0
                    OR word_count IS NULL OR word_count < 1 OR evidence.words IS NULL
                    OR len(evidence.words) <> word_count)
    THEN true ELSE error('expected one complete evidence match per nonempty sentence') END AS valid
FROM events;

CREATE TEMP TABLE words AS
SELECT sentence, position AS id, w.word, try_cast(w.head AS BIGINT) AS parent, w.rel,
       w.head IS NOT NULL OR w.rel IS NOT NULL AS annotated
FROM sentences, unnest(evidence.words) WITH ORDINALITY AS u(w, position);

-- Governor IDs index all sentence annotations. Distance uses the surface rank
-- among bunsetsu alone. A null governor needs an observed word root in the span.
CREATE TEMP TABLE bunsetsu AS
SELECT sentence, span_index - 1 AS span_id, s.governor, s.start AS first_word,
       s.end AS past_end, word_count,
       row_number() OVER by_position AS id,
       lag(s.end, 1, 0) OVER by_position AS previous_end,
       lead(s.start, 1, word_count) OVER by_position AS next_start
FROM sentences, unnest(evidence.spans) WITH ORDINALITY AS u(s, span_index)
WHERE s.kind = 'bunsetsu'
WINDOW by_position AS (PARTITION BY sentence ORDER BY s.start, span_index);

CREATE TEMP TABLE nodes AS
SELECT sentence, 'word' AS unit, id, parent, annotated,
       word = id AND coalesce(rel, '_') NOT IN ('', '_')
       AND (parent = 0) = (rel = 'root') AS valid
FROM words
UNION ALL
SELECT b.sentence, 'bunsetsu', b.id,
       CASE WHEN b.governor IS NULL AND EXISTS (
           SELECT 1 FROM words w WHERE w.sentence = b.sentence AND w.parent = 0
           AND w.rel = 'root' AND w.id BETWEEN b.first_word + 1 AND b.past_end
       ) THEN 0 ELSE g.id END,
       true, b.first_word = b.previous_end AND b.past_end = b.next_start
             AND b.first_word < b.past_end
FROM bunsetsu b LEFT JOIN bunsetsu g ON g.sentence = b.sentence AND g.span_id = b.governor;

-- Traversal starts at real roots. A disconnected cycle remains unreachable.
WITH RECURSIVE depths AS (
    SELECT sentence, unit, id, 0 AS depth FROM nodes WHERE parent = 0
    UNION ALL
    SELECT n.sentence, n.unit, n.id, d.depth + 1
    FROM depths d JOIN nodes n ON (n.sentence, n.unit, n.parent) = (d.sentence, d.unit, d.id)
), endpoints AS (
    SELECT sentence, unit, least(id, parent) AS position, 1 AS delta FROM nodes WHERE parent > 0
    UNION ALL
    SELECT sentence, unit, greatest(id, parent), -1 FROM nodes WHERE parent > 0
), boundaries AS (
    SELECT sentence, unit, position,
           sum(sum(delta)) OVER (PARTITION BY sentence, unit ORDER BY position) AS open_links
    FROM endpoints GROUP BY sentence, unit, position
), profiles AS (
    SELECT n.sentence, n.unit, count(*) AS unit_count, bool_or(annotated) AS annotated,
           count(*) FILTER (WHERE parent = 0) = 1 AND count(depth) = count(*)
           AND bool_and(coalesce(valid, false)) AS tree_ok,
           avg(abs(id - parent)) FILTER (WHERE parent > 0) AS mean_distance,
           avg(depth) AS mean_depth, max(depth) AS height
    FROM nodes n LEFT JOIN depths USING (sentence, unit, id) GROUP BY n.sentence, n.unit
), peaks AS (
    SELECT sentence, unit, max(open_links)::BIGINT AS peak FROM boundaries GROUP BY sentence, unit
)
SELECT sentence, source_name, document_sentence, unit, unit_count,
       CASE WHEN NOT coalesce(annotated, false) THEN 'unavailable'
            WHEN tree_ok THEN 'ok' ELSE 'invalid_tree' END AS status,
       CASE WHEN tree_ok THEN mean_distance END AS mean_distance,
       CASE WHEN tree_ok THEN mean_depth END AS mean_depth,
       CASE WHEN tree_ok THEN height END AS height,
       CASE WHEN tree_ok THEN coalesce(peak, 0) END AS peak_open_links
FROM sentences CROSS JOIN (VALUES ('word'), ('bunsetsu')) AS layers(unit)
LEFT JOIN profiles USING (sentence, unit) LEFT JOIN peaks USING (sentence, unit)
ORDER BY sentence, unit DESC;
