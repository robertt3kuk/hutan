; Hutan overlay (MIT OR Apache-2.0).
(char_literal) @string
(escape_sequence) @escape
[ (true) (false) ] @constant
(primitive_type) @type.builtin
(sized_type_specifier) @type.builtin
(parameter_declaration declarator: (identifier) @variable.parameter)
(binary_expression operator: _ @operator)
(unary_expression operator: _ @operator)
(assignment_expression operator: _ @operator)
(update_expression operator: _ @operator)
(conditional_expression [ "?" ":" ] @operator)
[ "(" ")" "[" "]" "{" "}" ] @punctuation.bracket
[ "," ";" ] @punctuation.delimiter
[ "goto" "register" "restrict" "_Atomic" "signed" "unsigned" "long" "short" ] @keyword
