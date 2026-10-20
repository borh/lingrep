; Comments and metadata
(comment) @comment

; Metadata: keys, values, and the two block keywords. Metadata is parsed
; syntax, not an opaque line, so a key highlights as a property and a value by
; its type rather than the whole region fading to a documentation comment.
(metadata_entry key: (identifier) @property)
(key_entry key: (identifier) @property)
(match_key_projection attribute: (attribute_key) @property)
(capture_key_projection
  capture: (capture_reference
    "$" @operator
    (identifier) @variable.member)
  component: (capture_component)? @constant.builtin
  attribute: (attribute_key) @property)
(lexical_term_projection) @constant.builtin
(lexical_segments_projection
  "segments" @function.builtin
  attribute: (attribute_key) @property)
(metadata_map_entry key: (identifier) @property)
(schema_entry key: (identifier) @property)
(metadata_string) @string
(metadata_number) @number
(metadata_boolean) @boolean
; `none` unsets a file default and is a builtin word, not an authored string,
; so it takes the constant face.
(metadata_none) @constant.builtin
(metadata_type) @type
(defaults_block "defaults" @keyword)
(defaults_block "end" @keyword)
(schema_block "schema" @keyword)
(schema_block "end" @keyword)

; Policy: a third block keyword, and a verb that is a keyword only inside it.
; The argument key highlights as a property for the same reason a metadata key
; does; an enum is a bare value, so it takes the constant face rather than the
; @variable one the catch-all `(identifier)` rule would otherwise give it.
(policy_block "policy" @keyword)
(policy_block "end" @keyword)
(policy_verb) @keyword
(policy_subject "all" @keyword)
(policy_subject "patterns" @keyword)
(policy_argument key: (identifier) @property)
(policy_map_entry key: (identifier) @property)
(policy_enum) @constant
(group_ref
  "%" @operator
  (identifier) @variable.member)

; Pattern definitions
(pattern_definition
  (definition_kind)? @keyword
  (identifier) @entity.name.function
  ":=" @keyword.operator.definition)

(lexicon_definition
  "lexicon" @keyword
  name: (identifier) @entity.name.function
  "by" @keyword
  projection: (attribute_key) @property
  ":=" @keyword.operator.definition
  member: (metadata_string) @string)


; Set declarations. The keyword takes the same face as `pattern`/`fragment`,
; and the name the same face as a definition name, because a reader scans for
; all three the same way.
(set_definition
  "set" @keyword
  (identifier) @entity.name.function
  ":=" @keyword.operator.definition)

; Test declarations. Same reasoning and same faces as `set_definition`: a
; test names a predicate, not an expression, but a reader still scans for it
; the way they scan for `pattern`, `fragment`, and `set`.
(test_definition
  "test" @keyword
  (identifier) @entity.name.function
  ":=" @keyword.operator.definition)

; Group declarations use the same faces as set/test definitions; the
; predicate key is a metadata key and its operators take the attribute-operator
; face.
(group_definition
  "group" @keyword
  (identifier) @entity.name.function
  ":=" @keyword.operator.definition)

(group_predicate
  key: (identifier) @property)

(group_operator) @operator

; Operators and punctuation. `&` and `|` are anonymous wherever they appear, so
; the token-test algebra's conjunction and disjunction are already covered; `!`
; is scoped to the node that can hold it, and is the one glyph in the algebra
; whose meaning diverges from its `!=` neighbour.
"&" @operator
"|" @operator
(test_negation "!" @operator)
(relation_negation) @operator
(anonymous_dependency_node) @constant.builtin
"." @punctuation.delimiter

; Node patterns
(node_pattern
  "[" @punctuation.bracket
  "]" @punctuation.bracket)

(key_projection_list
  "[" @punctuation.bracket
  "]" @punctuation.bracket)

(attribute
  (key) @property
  [(attribute_operator)
   (set_has_operator)
   (set_has_any_operator)
   (set_not_has_operator)
   (set_not_has_any_operator)] @operator
  [(value) (value_set)] @string)

(sentence_start) @punctuation.special
(sentence_end) @punctuation.special
(preceded_by "preceded_by" @keyword)
(any_order "any_order" @keyword)

; Regex patterns
(regex) @string.regex

; Bare patterns and references
(bare_element) @variable

(pattern_reference
  "@" @operator
  (identifier) @variable.member)

(set_reference
  "@" @operator
  (identifier) @variable.member)

; A test reference takes the same faces as the other two: `@X` has one
; referent whichever kind `X` is.
(test_reference
  "@" @operator
  (identifier) @variable.member)

; Special syntax
(named_group
  "(?<" @punctuation.special
  (identifier) @variable.parameter
  ">" @punctuation.special)

(parenthesized_pattern
  "(" @punctuation.bracket
  ")" @punctuation.bracket)

; Grouping inside `[…]`, which is a different node from the expression-level
; grouping above and would otherwise be the only unstyled bracket pair.
(parenthesized_test
  "(" @punctuation.bracket
  ")" @punctuation.bracket)

; Quantifiers
(quantifier) @operator

; Identifiers and literals
(identifier) @variable
