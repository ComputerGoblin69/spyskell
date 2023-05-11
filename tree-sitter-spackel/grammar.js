module.exports = grammar({
  name: "spackel",

  extras: $ => [/\s/, $.line_comment],

  word: $ => $.word,

  rules: {
    source_file: $ => repeat(choice($.macro_definition, $._instruction)),

    macro_definition: $ =>
      seq("macro", field("name", $.word), repeat($._instruction), "end"),

    _instruction: $ => choice($.number, $.word),

    number: $ => /[+-]?\d+/,

    word: $ => /[^#\s]+/,

    line_comment: $ => /#.*/,
  },
});
