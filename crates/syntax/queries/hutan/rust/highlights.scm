; Hutan overlay on the vendored highlights.scm: later patterns win over the vendored ones for the
; same node, so everything here corrects or completes upstream (MIT OR Apache-2.0).

; Numbers are numbers; `true`/`false` stay constants (upstream calls all literals constants).
(integer_literal) @number
(float_literal) @number

; Upstream's all-caps rule has a stray quote in its regex and never matches.
((identifier) @constant
 (#match? @constant "^[A-Z][A-Z\\d_]+$"))
(const_item name: (identifier) @constant)
(static_item name: (identifier) @constant)

(lifetime) @label
(shorthand_field_initializer (identifier) @property)
(field_initializer field: (field_identifier) @property)

(macro_invocation
  macro: (scoped_identifier
    name: (identifier) @function.macro)
  "!" @function.macro)

(binary_expression operator: _ @operator)
(unary_expression "!" @operator)
(unary_expression "-" @operator)
(assignment_expression "=" @operator)
(compound_assignment_expr operator: _ @operator)
(let_declaration "=" @operator)
(type_cast_expression "as" @keyword)
(range_expression [".." "..=" "..."] @operator)
(try_expression "?" @operator)
(closure_parameters "|" @operator)
"=>" @operator
"->" @operator
