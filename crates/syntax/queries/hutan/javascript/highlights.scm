; Hutan overlay (MIT OR Apache-2.0). Shared by JavaScript, TypeScript and TSX.
(class_declaration name: (_) @type)
(arrow_function parameter: (identifier) @variable.parameter)
(shorthand_property_identifier) @property
(statement_identifier) @label

; The vendored uppercase-is-a-constructor rule comes after its function rules and hides them.
(function_declaration name: (identifier) @function)
(function_expression name: (identifier) @function)
(call_expression function: (identifier) @function)
(new_expression constructor: (identifier) @type)
