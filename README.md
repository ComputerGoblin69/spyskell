# Spackel

Spackel is a stack-based programming language implemented in Rust.

## Usage

To interpret a Spackel program, run Spackel with the `run` command and provide
the name of the file:

```sh
cargo run run program.spkl
```

To compile a Spackel program to an executable, provide the `compile` command
instead of `run`. This produces an object file which will then have to be linked
with the prelude (and libc):

```sh
cargo run compile program.spkl
cc -o main prelude.o main.o
./main
```

The prelude can be compiled by running `make`.

The script `./compile` is provided for convenience and performs these steps for
you, including ensuring that the prelude and the compiler itself are up to date:

```sh
./compile program.spkl
./main
```

## Syntax

- Source files must be encoded as UTF-8.
- Instructions are separated by whitespace.
- Comments start with `#`.

## Data types

For now, there is only one data type: `i32`, the signed 32-bit integer.

## Instructions

### Constants

- Integers: at least one decimal digit, optionally preceded by a `+` or `-`.
- `ß`: pushes the number 1945.

### Math

- Binary operators (these take two numbers as arguments and produce one number):
  - `+`: addition.
  - `-`: subtraction.
  - `*`: multiplication.
  - `/`: division.
  - `%`: remainder after division. This is *not* the same as modulo when
    negative numbers are involved.
  - `+🤡`: silly addition. This is like regular addition with some exceptions:
    - $9+10 = 21$
    - $10+9 = 21$
    - $1+1 = 1$

### Stack manipulation

- `drop`: pops one element.
- `dup`: duplicates the top element.
- `swap`: swaps the top two elements.
- `over`: duplicates the second element onto the top of the stack, turning `a b`
  into `a b a`.
- `nip`: pops the second element.
- `tuck`: duplicates and tucks away the top element, turning `a b` into `b a b`.

### IO

- `println`: pops and prints the top element, followed by a new line.
- `print-char`: pops the top element, reinterprets it as unsigned, converts that
  to a Unicode scalar value, or `U+FFFD REPLACEMENT CHARACTER` in the case of an
  invalid code point, and prints it.
