# TOMLX Specification (v0.1)

TOMLX is a progressively enhanced superset of TOML used by `neomake`.
It adds three capabilities — **variables**, **value-level conditionals**,
and **built-in functions** — while guaranteeing that any valid
[TOML](https://toml.io/) document is a valid TOMLX document with
identical semantics.

This specification targets v0.1 of TOMLX as implemented in
`neomake-tomlx`. It is intentionally small; items marked *"Future"* are
reserved for later extensions.

---

## 1. Relationship to TOML

TOMLX is a **strict superset** of TOML:

- Every valid `.toml` file is also a valid `.tomlx` file.
- If a file contains no TOMLX sentinels (see §6), the evaluator
  short-circuits and hands the source directly to an underlying TOML
  parser. The resulting value is the same one a plain TOML parser would
  produce.

When TOMLX-specific syntax is present, the evaluator runs the pipeline
defined in §7.

---

## 2. Variables

A **variable declaration** is a line, outside any table header, of the
form:

```
$NAME = EXPR
```

where `NAME` is an identifier matching `[A-Za-z_][A-Za-z0-9_-]*` and
`EXPR` is a TOMLX expression (§4).

- Declarations are evaluated top-to-bottom; later declarations may
  reference earlier ones.
- The line is removed from the document before it is handed to the TOML
  parser, so declarations are invisible to TOML consumers.
- Redeclaration within the same file is allowed and shadows the
  previous value.

### References

Inside an expression, `${NAME}` (with or without surrounding string
quotes) resolves to the value of the variable `NAME`. Inside an
interpolation, the `$` prefix is redundant — `${NAME}` and, bare,
`NAME` both mean the same thing.

Referencing an undefined variable is an error:

```
error: undefined variable `profile`
```

---

## 3. String Interpolation

Inside **double-quoted** (template) strings, `${EXPR}` is replaced by
the result of evaluating `EXPR`, stringified per §5.

```
msg = "hello ${user_name}!"
```

- Escape the sequence with a preceding backslash: `"\${literal}"` is
  the five-character literal `${literal}`.
- **Single-quoted** (literal) strings perform no interpolation:
  `'hello ${user_name}'` is the literal 21-character string.

---

## 4. Expressions

TOMLX expressions extend TOML values with operators, conditionals, and
function calls. The full grammar is given in §8.

### Operators (precedence lowest → highest)

| Operator | Meaning                       | Operands              |
|----------|-------------------------------|-----------------------|
| `\|\|`   | short-circuit logical OR      | bool, bool            |
| `&&`     | short-circuit logical AND     | bool, bool            |
| `==`     | equality                      | any matching pair     |
| `!=`     | inequality                    | any matching pair     |
| `+`      | addition / concat / extend    | int+int, float+float, string+string, array+array |
| `!`      | logical NOT (unary)           | bool                  |
| `-`      | negation (unary)              | int, float            |

Parentheses `(` `)` override precedence.

### Conditionals

```
if COND { THEN_EXPR } else { ELSE_EXPR }
```

`COND` must evaluate to a boolean; both branches are expressions and
may have different types. Exactly one branch is evaluated.

*Future*: a block-level form, `if COND { [table.path] ... }`, that
conditionally includes whole TOML tables.

### Arrays

`[EXPR, EXPR, ...]`. Every element is evaluated; the resulting array is
a plain TOML array.

---

## 5. Built-in Functions

All built-ins are pure as seen from the TOML parser: they are evaluated
during the TOMLX phase and their result becomes part of the emitted
TOML document.

### `env(name)` / `env(name, default)`

Returns the value of the environment variable `name` (a string). If the
variable is not set:

- With one argument — evaluation fails with a clear error.
- With two arguments — `default` is returned (must be a string).

### `glob(pattern)`

Returns a sorted array of strings listing every file under the project
root whose path (relative to that root) matches `pattern`. The pattern
follows the [`globset`](https://docs.rs/globset) syntax, with `**`
matching any number of path components.

The cache directory (`.neomake/`) is never included.

### `exec(command)`

Runs `command` via the platform's default shell with the project root
as the working directory, captures its stdout, and returns it as a
trimmed string. A non-zero exit status is a hard error.

The shell is `sh -c` on Unix-likes; on Windows it is `pwsh` (preferred,
UTF-8 by default), then `powershell.exe`, then `cmd.exe /S /C` as a
fallback. Override with the `NEOMAKE_SHELL` environment variable
(whitespace-tokenized argv; the command string is appended as the
final argument).

> **Cache caveat.** `exec()` is not re-evaluated as part of a task's
> cache key; its value is captured at load time. If you need command
> output to participate in cache invalidation, write it to a file and
> list that file under the task's `inputs`.

### Type coercion

TOMLX does not perform implicit numeric-to-string or
string-to-numeric coercion. Operators require exactly matching types
with the exceptions documented in §4. Use an explicit built-in if you
need to convert (none currently provided — *Future*).

---

## 6. Sentinels That Activate TOMLX Mode

The evaluator enters the TOMLX pipeline when any of the following are
present in the source:

- A top-level line matching `$IDENT = ...`.
- The substring `${`.
- The substring `= if ` (value-level conditional).
- A value starting with one of `env(`, `glob(`, `exec(`.

Otherwise the file is parsed directly as TOML.

This rule implies a small caveat: a plain TOML file containing `${X}`
*inside a string literal* will enter TOMLX mode if it also declares any
variables. Files that do not use TOMLX features are always parsed as
plain TOML and remain unchanged.

---

## 7. Evaluation Pipeline

```
source
  │
  ▼  lexer
tokens
  │
  ▼  parser
AST
  │
  ▼  evaluator  (variable scope, built-ins)
toml::Value
```

Concretely:

1. The **document scanner** walks the source line by line, identifying
   variable declarations and key/value lines whose RHS contains a
   TOMLX sentinel.
2. For each such RHS, the expression is lexed and parsed into an AST.
3. The evaluator resolves variables, evaluates operators, dispatches
   built-ins, and produces a `toml::Value` for each expression.
4. The scanner re-emits the document with evaluated values rendered as
   TOML literals and variable-declaration lines removed.
5. The rebuilt document is parsed by a standard TOML parser to produce
   the final value tree.

The pipeline is fully deterministic: given the same source, the same
environment, and the same working directory, it produces the same
output.

---

## 8. Grammar (EBNF)

```ebnf
tomlx_document  = { line } ;
line            = var_decl
                | key_value_line
                | toml_line ;

var_decl        = [ ws ] "$" ident ws "=" ws expr { ws } [ comment ] newline ;
key_value_line  = toml_key ws "=" ws rhs { ws } [ comment ] newline ;
rhs             = expr | toml_value ;    (* chosen by sentinel check *)

expr            = or_expr ;
or_expr         = and_expr { "||" and_expr } ;
and_expr        = eq_expr  { "&&" eq_expr } ;
eq_expr         = add_expr { ( "==" | "!=" ) add_expr } ;
add_expr        = unary    { "+" unary } ;
unary           = "!" unary | "-" unary | primary ;
primary         = literal
                | var_ref
                | call
                | array
                | if_expr
                | "(" expr ")" ;

var_ref         = "$" ident
                | "${" ident "}"
                | ident ;                (* only inside expression context *)

call            = ident "(" [ expr { "," expr } [ "," ] ] ")" ;
array           = "[" [ expr { "," expr } [ "," ] ] "]" ;
if_expr         = "if" expr "{" expr "}" "else" "{" expr "}" ;

literal         = int | float | bool | template_string | literal_string ;
template_string = '"' { tmpl_char | "${" expr "}" } '"' ;
literal_string  = "'" { any_char_except_quote_and_newline } "'" ;

ident           = ( letter | "_" ) { letter | digit | "_" | "-" } ;
```

`toml_line`, `toml_key`, and `toml_value` refer to the corresponding
productions from the [TOML 1.0](https://toml.io/en/v1.0.0) grammar.

---

## 9. Determinism and Limits (v0.1)

- Variable declarations and TOMLX-evaluated values must fit on a single
  physical source line. Multi-line TOML values (e.g. multi-line arrays
  or strings) are passed through verbatim and cannot reference TOMLX
  constructs.
- `exec()` is evaluated once per configuration load.
- Block-level `if` (conditionally including whole tables) is not yet
  supported; use a value-level conditional or split configurations into
  multiple files.

---

## 10. Versioning

This document describes TOMLX v0.1. Future versions will be
backwards-compatible: a v0.1 document must remain valid in v0.x for
`x ≥ 1`. The on-disk cache format is versioned independently; see
`neomake-core::cache::CACHE_FORMAT_VERSION`.
