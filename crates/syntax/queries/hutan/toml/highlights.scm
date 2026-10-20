; Hutan overlay (MIT OR Apache-2.0): upstream colors every bare key as a type; keys of pairs are
; properties and only table names are types.
(table (bare_key) @type)
(table (dotted_key (bare_key) @type))
(table_array_element (bare_key) @type)
(table_array_element (dotted_key (bare_key) @type))

; Upstream captures the whole pair as a property; only the key is one.
(pair) @none
(pair (bare_key) @property)
(pair (dotted_key (bare_key) @property))
(pair (quoted_key) @property)
