; Hutan overlay (MIT OR Apache-2.0): later patterns win over the vendored ones.
(class_definition name: (identifier) @type)
(parameters (identifier) @variable.parameter)
(default_parameter name: (identifier) @variable.parameter)
(typed_parameter (identifier) @variable.parameter)
(typed_default_parameter name: (identifier) @variable.parameter)
(lambda_parameters (identifier) @variable.parameter)
(keyword_argument name: (identifier) @variable.parameter)

; Upstream's generic `attribute` rule comes after its method-call rule and hides it.
(call function: (attribute attribute: (identifier) @function.method))

((identifier) @variable.builtin
 (#match? @variable.builtin "^(self|cls)$"))

(decorator "@" @attribute)
(import_statement name: (dotted_name (identifier) @type))
(import_from_statement module_name: (dotted_name (identifier) @type))
