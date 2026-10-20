; Hutan overlay (MIT OR Apache-2.0). Upstream puts `(identifier) @variable` after its call and
; definition rules, which only works if the first pattern wins; here later patterns win, so the
; specific rules are repeated.
(call_expression function: (identifier) @function)
(call_expression
  function: (selector_expression
    field: (field_identifier) @function.method))
(function_declaration name: (identifier) @function)
(method_declaration name: (field_identifier) @function.method)
(method_elem name: (field_identifier) @function.method)

(parameter_declaration name: (identifier) @variable.parameter)
(variadic_parameter_declaration name: (identifier) @variable.parameter)
(package_identifier) @type
(label_name) @label
(const_spec name: (identifier) @constant)

[ "(" ")" "[" "]" "{" "}" ] @punctuation.bracket
[ "," "." ":" ";" ] @punctuation.delimiter
