-- Run in DuckDB after SET VARIABLE matches = '/path/to/matches.jsonl'.
-- Use one complete, unfiltered ling --json matches --evidence run of
-- patterns/psycholinguistics.lx. Sentence ordinals belong to that run.
CREATE TEMP VIEW events AS
SELECT json->>'schema' AS schema_id, json->>'type' AS event, json->'data' AS d
FROM read_ndjson_objects(getvariable('matches'));

SELECT CASE WHEN count(*) FILTER (schema_id IS DISTINCT FROM 'lingrep.query/v2') = 0
             AND count(*) FILTER (event = 'run' AND (d->>'mode') = 'matches') = 1
             AND count(*) FILTER (event = 'run') = 1
             AND count(*) FILTER (event = 'match' AND json_type(d->'evidence') IS DISTINCT FROM 'OBJECT') = 0
             AND count(*) FILTER (event = 'complete' AND (d->>'coverage') = 'full') = 1
             AND count(*) FILTER (event = 'complete') = 1
            THEN 'complete' ELSE error('expected a complete ling --json matches --evidence run') END AS input
FROM events;

CREATE TEMP TABLE hits AS
SELECT d->>'pattern' AS pattern, (d->>'corpus_sentence_ordinal')::BIGINT AS sentence,
       (d->'span'->>'start')::BIGINT AS first_token, (d->'span'->>'end')::BIGINT AS past_end,
       d->>'surface' AS surface, d->'tokens'->0->'attributes'->>'lemma' AS lemma,
       d->'whole_span_ids'->'bunsetsu' AS bunsetsu_ids,
       CASE WHEN (d->>'pattern') = 'reading_bunsetsu' THEN d->'evidence'->'spans' END AS spans
FROM events WHERE event = 'match';

-- A word sharing both positions is the target itself. Each other type
-- can share at most one position, so these two families are disjoint.
WITH vocabulary AS (
    SELECT surface, count(*) AS frequency FROM hits
    WHERE pattern = 'two_kanji_type' GROUP BY surface
)
SELECT surface, frequency,
       count(*) OVER (PARTITION BY left(surface, 1)) - 1 AS same_first_neighbors,
       count(*) OVER (PARTITION BY right(surface, 1)) - 1 AS same_last_neighbors,
       same_first_neighbors + same_last_neighbors AS neighborhood_size
FROM vocabulary ORDER BY surface;

WITH spellings AS (
    SELECT lemma, surface, count(*) AS frequency FROM hits
    WHERE pattern = 'noun_spelling' AND lemma IS NOT NULL GROUP BY lemma, surface
)
SELECT *, frequency / sum(frequency) OVER (PARTITION BY lemma) AS spelling_share
FROM spellings ORDER BY lemma, surface;

-- A sentence-local annotation ID indexes the evidence array.
CREATE TEMP TABLE units AS
SELECT sentence, id.value::BIGINT AS bunsetsu_id, first_token, past_end, surface,
       ((spans->(id.value::BIGINT))->>'governor')::BIGINT AS governor_id
FROM hits, LATERAL json_each(bunsetsu_ids) id
WHERE pattern = 'reading_bunsetsu'
UNION ALL
-- Earlier query/v2 producers expose the IDs through the evidence array alone.
SELECT sentence, s.key::BIGINT, first_token, past_end, surface,
       (s.value->>'governor')::BIGINT
FROM hits, LATERAL json_each(spans) s
WHERE pattern = 'reading_bunsetsu' AND bunsetsu_ids IS NULL
  AND (s.value->>'kind') = 'bunsetsu' AND (s.value->>'start')::BIGINT = first_token
  AND (s.value->>'end')::BIGINT = past_end;

-- Classify displayed Unicode code points. Prolonged sound mark ー counts
-- with katakana. Whitespace, punctuation and other scripts break runs.
-- The list retains these separators and crosses token boundaries.
WITH loads AS (
    SELECT g.sentence, g.bunsetsu_id, count(d.bunsetsu_id) AS earlier_dependents
    FROM units g LEFT JOIN units d
      ON d.sentence = g.sentence AND d.governor_id = g.bunsetsu_id AND d.past_end <= g.first_token
    GROUP BY g.sentence, g.bunsetsu_id
), tokens AS (
    SELECT u.sentence, u.bunsetsu_id,
           count(*) FILTER (h.pattern = 'kanji_token') AS kanji_tokens,
           count(*) FILTER (h.pattern = 'hiragana_token') AS hiragana_tokens,
           count(*) FILTER (h.pattern = 'katakana_token') AS katakana_tokens
    FROM units u LEFT JOIN hits h ON h.sentence = u.sentence
      AND h.first_token >= u.first_token AND h.past_end <= u.past_end
      AND h.pattern IN ('kanji_token', 'hiragana_token', 'katakana_token')
    GROUP BY u.sentence, u.bunsetsu_id
)
SELECT u.sentence, u.bunsetsu_id, u.surface, loads.earlier_dependents,
       tokens.kanji_tokens, tokens.hiragana_tokens, tokens.katakana_tokens,
       regexp_extract_all(u.surface, '\p{Han}+|\p{Hiragana}+|[\p{Katakana}ー]+|[^\p{Han}\p{Hiragana}\p{Katakana}ー]+') AS character_runs,
       length(regexp_replace(u.surface, '[^\p{Han}]', '', 'g')) AS kanji_characters,
       length(regexp_replace(u.surface, '[^\p{Hiragana}]', '', 'g')) AS hiragana_characters,
       length(regexp_replace(u.surface, '[^\p{Katakana}ー]', '', 'g')) AS katakana_characters
FROM units u JOIN loads USING (sentence, bunsetsu_id) JOIN tokens USING (sentence, bunsetsu_id)
ORDER BY u.sentence, u.bunsetsu_id;

-- One candidate fixation target per occurrence. Missing bunsetsu annotations
-- leave the position unknown. Eye movements must be aligned independently.
SELECT h.sentence, h.first_token, h.pattern, h.surface, length(h.surface) AS characters,
       h.first_token = u.first_token AS bunsetsu_initial
FROM hits h LEFT JOIN units u ON h.sentence = u.sentence
  AND h.first_token >= u.first_token AND h.past_end <= u.past_end
WHERE h.pattern IN ('kanji_initial', 'hiragana_only')
ORDER BY h.sentence, h.first_token;

-- Maximal adjacent pure-script token runs. A missing index marks a mixed
-- token or another script. Partitioning by unit prevents cross-boundary runs.
WITH classified AS (
    SELECT u.sentence, u.bunsetsu_id, h.pattern, h.first_token, h.surface,
           h.first_token - row_number() OVER (
               PARTITION BY u.sentence, u.bunsetsu_id, h.pattern ORDER BY h.first_token
           ) AS island
    FROM units u JOIN hits h ON h.sentence = u.sentence
      AND h.first_token >= u.first_token AND h.past_end <= u.past_end
    WHERE h.pattern IN ('kanji_token', 'hiragana_token', 'katakana_token')
)
SELECT sentence, bunsetsu_id, pattern, min(first_token) AS start_token,
       count(*) AS token_count, list(surface ORDER BY first_token) AS tokens
FROM classified GROUP BY sentence, bunsetsu_id, pattern, island
ORDER BY sentence, start_token;

-- Each candidate is grounded at the embedded verb. Keep the UD witnesses
-- so a reviewer can check the head noun, coargument and its case marker.
SELECT (d->>'corpus_sentence_ordinal')::BIGINT AS sentence, d->>'pattern' AS candidate,
       d->>'surface' AS verb, d->'graph_bindings' AS witnesses
FROM events WHERE event = 'match'
  AND (d->>'pattern') IN ('subject_gap_candidate', 'object_gap_candidate')
ORDER BY sentence, (d->'span'->>'start')::BIGINT;

-- The nullable numeric binding keeps unknown observations distinct from zero.
SELECT (b.value->>'value')::BIGINT AS nominal_dependents, count(*) AS verbs
FROM events, LATERAL json_each(d->'graph_bindings') b
WHERE event = 'match' AND (d->>'pattern') = 'nominal_load' AND (b.value->>'name') = 'nominals'
GROUP BY nominal_dependents ORDER BY nominal_dependents;
