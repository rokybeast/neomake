# neomake

A progressively enhanced build orchestrator in Rust. `neomake` reads a
TOML (or TOMLX) configuration file, builds a dependency DAG, runs tasks
concurrently on a `tokio` executor, and skips unchanged work via a
SHA-256 content-addressable cache.

## Highlights

- **TOML in, TOML out.** Your build file starts as plain TOML. When you
  need more, opt in to TOMLX — a strict superset adding variables,
  value-level conditionals, and a small library of built-ins
  (`env`, `glob`, `exec`).
- **Parallel by default.** Tasks with no dependency run concurrently.
  Concurrency is capped with `--concurrency N` (default: number of
  CPUs).
- **Content-addressable cache.** Each task's cache key is a SHA-256 of
  its command, env, sorted input file hashes, and the cache keys of its
  upstream dependencies, so a change anywhere in the transitive input
  closure invalidates every dependent.
- **Clear diagnostics.** Cycle detection reports the concrete cycle
  path; every config error carries a file name and line number.

## Workspace layout

```
neomake/             CLI binary
neomake-core/        task model, DAG builder, cache, tokio executor
neomake-tomlx/       TOMLX lexer, parser, evaluator
examples/            basic.toml, advanced.tomlx
docs/tomlx-spec.md   formal TOMLX spec
```

## Installation

Requires a recent stable Rust toolchain (1.75+).

```bash
cargo install --path neomake
```

A `cargo build --release` at the workspace root produces
`target/release/neomake`.

## Quickstart

Create `neomake.toml`:

```toml
[tasks.build]
command = "cargo build --release"
inputs  = ["src/**/*.rs", "Cargo.toml"]
outputs = ["target/release/myapp"]

[tasks.test]
command = "cargo test"
deps    = ["build"]
```

Then:

```bash
neomake run               # builds all leaves (test → build)
neomake run build         # builds just `build` and its deps
neomake list              # shows the DAG in topological order
neomake cache status      # prints cache entry count and size
neomake cache clean       # empties the cache directory
```

### CLI reference

```
neomake [OPTIONS] <SUBCOMMAND>

Subcommands:
  run [TASK]...            Run tasks (defaults to all tasks)
  list                     Print tasks in topological order
  cache clean              Remove every cache entry
  cache status             Print cache size and entry count

Options:
  -c, --config <FILE>      Path to config; auto-detects neomake.toml or neomake.tomlx
      --concurrency <N>    Max parallel shell commands (default: num_cpus)
      --no-cache           Treat every task as a cache miss
  -q, --quiet              Suppress per-task stdout/stderr forwarding
```

Set `NEOMAKE_LOG=debug` to turn on `tracing` diagnostics.

## TOMLX at a glance

| Feature               | Syntax                                                      |
|-----------------------|-------------------------------------------------------------|
| Variable declaration  | `$profile = "release"`                                      |
| Variable reference    | `${profile}` (in strings) / `$profile` (in expressions)     |
| String interpolation  | `"target/${profile}/app"`                                   |
| String concatenation  | `"cargo build" + " --release"`                              |
| Conditional value     | `if ${ci} { "make ci" } else { "make" }`                    |
| Environment access    | `env("PROFILE", "debug")`                                   |
| File globbing         | `glob("src/**/*.rs")`                                       |
| Command execution     | `exec("git rev-parse HEAD")`                                |

See [`docs/tomlx-spec.md`](docs/tomlx-spec.md) for the full grammar and
semantics, and [`examples/advanced.tomlx`](examples/advanced.tomlx) for
a complete example.

## Architecture

```
┌───────────────────┐        ┌──────────────────────────┐
│  *.toml / *.tomlx │──────▶│   neomake-tomlx          │
└───────────────────┘        │  (lexer / parser / eval) │
                             └───────────┬──────────────┘
                                         │ toml::Value
                                         ▼
                             ┌──────────────────────────┐
                             │   neomake-core           │
                             │   TaskSet → DAG          │
                             │         │                │
                             │         ▼                │
                             │   Executor (tokio)       │
                             │         │                │
                             │         ▼                │
                             │   Cache (.neomake/cache) │
                             └──────────────────────────┘
```

- **DAG construction** uses Kahn's algorithm for topological sorting; if
  a cycle is detected, an iterative DFS recovers the concrete cycle
  path (e.g. `a -> b -> c -> a`) and reports it.
- **Cache keys** are SHA-256 over a versioned canonical byte layout
  (`neomake-cache-v1`) built from the command, env, sorted input file
  hashes, and upstream dep keys. See `neomake-core/src/cache.rs` for the
  exact format.
- **Parallel execution** uses one tokio task per DAG node, a per-task
  `watch` channel to propagate outcomes to dependents, and a
  `Semaphore` to cap concurrent shell commands. Cache hits bypass the
  semaphore entirely.

## Scope

**In scope**: TOML / TOMLX config parsing, DAG-based parallel
execution, content-addressable local caching, CLI commands listed
above, cycle detection with clear errors.

**Out of scope for this version**: remote/distributed caching, plugin
systems, watch mode, GUI/TUI, Windows (Linux/macOS only).

## License

Dual-licensed under MIT or Apache-2.0.
