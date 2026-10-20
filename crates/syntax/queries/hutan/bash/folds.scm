; Foldable nodes (MIT OR Apache-2.0).
(function_definition body: (compound_statement) @fold)
[
  (if_statement)
  (for_statement)
  (c_style_for_statement)
  (while_statement)
  (case_statement)
  (subshell)
  (heredoc_body)
] @fold
