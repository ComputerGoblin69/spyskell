(line_comment) @comment.line

(number) @constant.numeric

(
 (word) @function.builtin
 (#match? @function.builtin "^(println|print-char|\\+|-|\\*|/|%|ß|drop|dup|swap|over|nip)$")
)

(word) @variable
