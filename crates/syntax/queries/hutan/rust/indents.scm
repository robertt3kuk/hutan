; Indentation scopes (MIT OR Apache-2.0): lines after the first row of an @indent node are one
; level deeper; a line that starts with an @outdent node goes one level back.
[
  (block)
  (declaration_list)
  (field_declaration_list)
  (ordered_field_declaration_list)
  (enum_variant_list)
  (match_block)
  (use_list)
  (field_initializer_list)
  (arguments)
  (parameters)
  (token_tree)
  (array_expression)
  (tuple_expression)
  (type_arguments)
] @indent

[ "}" "]" ")" ] @outdent
