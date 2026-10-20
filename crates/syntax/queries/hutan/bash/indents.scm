; Indentation scopes (MIT OR Apache-2.0).
[
  (function_definition)
  (compound_statement)
  (subshell)
  (if_statement)
  (for_statement)
  (c_style_for_statement)
  (while_statement)
  (case_statement)
  (case_item)
] @indent

[ (elif_clause) (else_clause) ] @outdent
[ "fi" "done" "esac" "}" ")" ] @outdent
