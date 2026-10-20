; Hutan overlay (MIT OR Apache-2.0). Upstream marks keys before `(string) @string`, which only
; works if the first pattern wins; here later patterns win.
(pair key: (_) @property)

[ "[" "]" "{" "}" ] @punctuation.bracket
[ "," ":" ] @punctuation.delimiter
