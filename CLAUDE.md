# pdf-core — Claude Instructions

## Project

Rust rebuild of ONLYOFFICE PdfFile C++ module. Targets native + WASM (`wasm32-unknown-unknown`).
Crate: `pdf-core` (cdylib + rlib). Deps: nom, thiserror, flate2, weezl, log.

## Workflow Rules

1. **Always write unit tests** — every new function or module gets tests. Parser functions need both happy-path and error-path tests with crafted bytes.
2. **Follow Rust best practices** — idiomatic patterns, proper ownership, no unnecessary clones, leverage the type system.
3. **Always write doc comments** — every public function gets a `///` doc comment describing what it does, its parameters, and return value. Keep it concise.
4. **Ask when out of spec** — if a task is ambiguous or goes beyond what was specified, stop and ask before proceeding.
5. **Ask on trade-offs** — when facing a design trade-off (performance vs readability, flexibility vs simplicity, etc.), present the options and wait for confirmation before implementing.
6. **Write implementation report** — after completing any module, feature, or non-trivial fix, write `.doc/<scope>-<YYYY-MM-DD>.md` covering: what was built, design decisions, implementation date, tests added, and known limitations.

## Development Rules (R1–R10)

- **R1 No Panic:** All public fns return `Result<T, PdfError>`. No `unwrap()`/`expect()` outside `#[cfg(test)]`.
- **R2 WASM-Safe:** Every dep must be WASM-compatible. Run `cargo build --target wasm32-unknown-unknown` after Cargo.toml changes. Native-only code behind `#[cfg(not(target_arch = "wasm32"))]`.
- **R3 Feature Flags:** `render`, `writer`, `crypto`, `wasm` are optional features. Default minimal.
- **R4 One Module = One Concern:** No file >800 lines. Split at 600. `mod.rs` only declares + re-exports.
- **R5 Test-Driven Parsers:** Every `fn parse_*` needs happy-path + error-path test.
- **R6 Real PDF Integration Tests:** `tests/fixtures/` with minimal.pdf, multipage.pdf, encrypted.pdf.
- **R7 Byte-Offset Errors:** Every `PdfError` variant carries `offset: usize`.
- **R8 No unsafe Without Comment:** All `unsafe` blocks need `// SAFETY:` comment.
- **R9 Logging Not Printing:** Use `log::warn!`/`log::debug!`. Never `println!` in library code.
- **R10 Semantic Versioning:** Breaking changes bump minor (pre-1.0) with CHANGELOG.md entry.
- **R11 Implementation Reports:** After completing any module, feature, parser, or non-trivial fix, write a report to `.doc/<scope>-<YYYY-MM-DD>.md`. The report must include: what was implemented (functions, types, modules), why each design decision was made, date of implementation, test coverage added, and any known limitations or follow-up work.

## Implementation Report Format

Every completed unit of work (module, feature, parser, significant fix) must produce a report at `.doc/<scope>-<YYYY-MM-DD>.md`.

Required sections:

```markdown
# <Scope> — Implementation Report

**Date:** YYYY-MM-DD
**Scope:** <module or feature name>

## What Was Implemented
- List of functions, types, and modules added or changed

## Design Decisions
- Each non-obvious decision with its rationale

## Test Coverage
- Test names and what they cover (happy-path / error-path)

## Known Limitations / Follow-up
- Any deferred work, edge cases not yet handled, or open questions
```

File naming: use the Rust module or feature name as scope (e.g. `parser-xref-2026-05-23.md`, `crypto-rc4-2026-05-23.md`).

## Verification (run before reporting task complete)

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --target wasm32-unknown-unknown
```

## Commit Style

- Format: `type(scope): description` (e.g. `feat(parser): add object stream decoding`)
- Types: feat, fix, refactor, test, docs, chore
- Stage specific files, not `git add .`

## Code Patterns

- Errors: use `PdfError` variants with byte offset
- Parsing: nom combinators, return `IResult` internally, wrap in `Result<T, PdfError>` at public boundary
- Naming: `snake_case` fns, `PascalCase` types, `SCREAMING_SNAKE` constants
- Modules: one concern per file, re-export public API from `mod.rs`
