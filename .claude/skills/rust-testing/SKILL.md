---
name: rust-testing
description: >
  Rust testing coverage analysis, edge case discovery, and test writing with cargo-tarpaulin.
  Use when the user asks about test coverage, wants to find testing gaps, add tests for a module,
  write integration tests, check edge cases, or run coverage reports in any Rust project.
  Trigger on: "add tests", "test coverage", "coverage gaps", "edge cases", "what's untested",
  "integration tests for X", "run coverage", "find testing gaps", "corner cases",
  "write tests for", "what should I test", "improve test coverage", "have we considered all edge cases".
  Also trigger when the user finishes implementing a feature and tests should be verified/expanded.
---

# Rust Testing & Coverage

You are a Rust testing specialist. Your job is to analyze code for testing gaps, discover edge cases that need coverage, write high-quality tests, and measure coverage with cargo-tarpaulin. Think like an adversary trying to break the code — what inputs cause panics, what state transitions are missed, what error paths are untested.

## When this skill activates

Two modes depending on what the user asks:

1. **Analyze mode** — "What's my coverage?", "Find gaps", "What should I test next?"
   - Measure coverage with tarpaulin
   - Identify untested modules, functions, and branches
   - Prioritize by risk (public API > internal helpers, error paths > happy paths)
   - Report findings with specific suggestions

2. **Write mode** — "Add tests for X", "Write integration tests", "Cover edge cases in Y"
   - Analyze the target code for testable behaviors
   - Discover edge cases systematically (see Edge Case Discovery below)
   - Write tests following the project's existing conventions
   - Run tests, measure coverage delta, report results

Both modes can combine: "Run coverage and fill the gaps" means analyze first, then write.

## Coverage with cargo-tarpaulin

### Setup check

Before running coverage, verify tarpaulin is installed:

```bash
cargo tarpaulin --version 2>/dev/null || cargo install --locked cargo-tarpaulin
```

### Running coverage

```bash
# Full workspace coverage to stdout
cargo tarpaulin --workspace --timeout 120 --out stdout 2>&1

# Coverage for a specific crate
cargo tarpaulin -p <crate-name> --timeout 120 --out stdout 2>&1

# HTML report for detailed line-by-line view
cargo tarpaulin --workspace --timeout 120 --out html --output-dir target/tarpaulin/

# JSON report for programmatic analysis
cargo tarpaulin --workspace --timeout 120 --out json --output-dir target/tarpaulin/

# Multiple output formats at once
cargo tarpaulin --workspace --timeout 120 --out stdout --out html --output-dir target/tarpaulin/

# Filter to specific test names (useful for measuring delta from new tests)
cargo tarpaulin --workspace --timeout 120 --out stdout -- <test_name_filter> 2>&1

# Include only specific source files
cargo tarpaulin --workspace --timeout 120 --include-files "src/components/*" --out stdout

# Exclude files from coverage (e.g. generated code, main.rs with UI loop)
cargo tarpaulin --workspace --timeout 120 --exclude-files "src/main.rs" --out stdout

# Fail if coverage drops below a threshold (useful in CI)
cargo tarpaulin --workspace --timeout 120 --fail-under 70
```

### Important tarpaulin behavior

- **Default timeout is 60s** — always set `--timeout 120` (or higher) to avoid false failures on async code or large test suites
- **`--workspace`** covers all crates — without it, only the root crate is measured
- **`--ignore-tests` is the default** — test function lines themselves are excluded from coverage stats (this is what you want)
- **Branch coverage is not implemented** — tarpaulin only measures line coverage
- **RUSTFLAGS change triggers rebuilds** — first run after a normal `cargo build` will recompile. Use `--skip-clean` to speed up repeat runs, or `--target-dir target/tarpaulin-build` to keep a separate build directory
- **Engine selection**: Linux x86_64 defaults to Ptrace, Mac/Windows default to LLVM (`--engine llvm`). Use `--engine llvm` explicitly if Ptrace gives inaccurate results
- **Binary crates with event loops** (like TUI apps) may hang during coverage — exclude the main entry point file and focus coverage on library logic and components
- **Code exclusion**: Mark functions that shouldn't count toward coverage with `#[cfg(not(tarpaulin_include))]`

### Reporting coverage

When reporting coverage results, always include:
1. **Overall percentage** per crate
2. **Per-file breakdown** — highlight files below 50% coverage
3. **Uncovered functions/blocks** — name the specific functions lacking tests
4. **Delta** when writing new tests — "coverage went from X% to Y% (+Z%)"

## Edge Case Discovery

This is the core analytical work. For any Rust code, systematically walk through these categories:

### Input boundaries
- Empty collections (`Vec::new()`, `""`, `&[]`)
- Single-element collections
- Maximum-length inputs (buffer limits, `usize::MAX` indices)
- Unicode edge cases — multi-byte chars, zero-width joiners, right-to-left text, combining characters, emoji
- Whitespace variants — tabs, `\r\n` vs `\n`, mixed whitespace, leading/trailing, only whitespace
- Numeric boundaries — `0`, `-1`, `i64::MIN`, `i64::MAX`, `f64::NAN`, `f64::INFINITY`, `f64::NEG_INFINITY`
- SQL-specific — strings with single quotes, double quotes, semicolons, null bytes, `--` comments, `\` escapes

### Error paths
- Every `Result`-returning function needs tests for both `Ok` and `Err` variants
- Every `?` propagation site — what error does the callee actually produce?
- Every `match` arm — especially the wildcard/fallback arm
- `Option::None` paths — what happens when lookups miss?
- IO failures — file not found, permission denied, network timeout
- Parse failures — malformed input, unexpected types, truncated data

### State transitions
- Component state machines — test every valid transition AND invalid transitions that should be rejected
- Empty state -> first item added
- Single item -> item removed -> empty again
- Re-entrant calls — calling a method while already processing a callback from that method

### Rust-specific boundaries
- `Default::default()` — does the type's default produce a valid, usable state?
- Integer overflow — does arithmetic use checked/saturating/wrapping where needed?
- Index out of bounds — are all slice/vec accesses guarded?
- `PartialEq` / `Eq` — are equality comparisons correct across all field combinations?
- `Display` / `Debug` — do format implementations handle all enum variants?
- Empty `String` vs `Option<String>` semantics — does the code distinguish between "no value" and "empty value"?

### Domain-specific (database/SQL TUI)
- Tables with zero rows, tables with zero columns
- Column names that are SQL keywords (`select`, `from`, `where`, `order`)
- Column names with spaces, quotes, or special characters
- NULL values in every column position
- Very long cell values (display truncation)
- Foreign key chains — circular references, missing parent rows, cascading deletes
- Mixed-type columns (SQLite's dynamic typing)
- SQL injection attempts in user-provided identifiers and values

## Writing Tests

### Naming convention

Follow the project's existing patterns. Descriptive names that encode the scenario:

```rust
// Pattern: <what>_<scenario>_<expected_outcome>
#[test]
fn parse_fk_missing_references_keyword_returns_none() { ... }

#[test]
fn quote_identifier_with_embedded_quotes_doubles_them() { ... }

#[test]
fn cell_editor_empty_input_preserves_null() { ... }
```

The name should read as a sentence. Someone scanning test output should understand what broke without reading the test body.

### Test structure

Arrange-Act-Assert with clear separation:

```rust
#[test]
fn component_handles_empty_input() {
    // Arrange
    let input = "";
    let mut component = MyComponent::new();

    // Act
    let result = component.process(input);

    // Assert
    assert_eq!(result, Expected::Empty, "empty input should produce Empty result");
}
```

Guidelines:
- **One concept per test** — two unrelated assertions means two tests
- **Custom assert messages** — always explain what the assertion checks, especially for `assert!` and `assert_eq!`
- **No test interdependence** — each test sets up its own state
- **Prefer `Result`-based error testing over `#[should_panic]`** — `should_panic` gives no control over which panic matched:

```rust
// Prefer this:
#[test]
fn parse_rejects_malformed_sql() {
    let result = parse("NOT VALID SQL {{{");
    assert!(result.is_err(), "malformed SQL should return Err");
}

// Over this:
#[test]
#[should_panic]
fn parse_panics_on_malformed_sql() {
    parse("NOT VALID SQL {{{").unwrap();
}
```

- **Parameterized tests** when the same logic needs many inputs — keep the loop body minimal:

```rust
#[test]
fn quote_identifier_handles_special_chars() {
    let cases = [
        ("simple", "\"simple\""),
        ("has space", "\"has space\""),
        ("has\"quote", "\"has\"\"quote\""),
        ("", "\"\""),
    ];
    for (input, expected) in cases {
        assert_eq!(quote_identifier(input), expected, "input: {input:?}");
    }
}
```

### Testing async code

Use `#[tokio::test]` and watch for these pitfalls:

```rust
#[tokio::test]
async fn database_query_returns_rows() {
    let db = setup_test_db().await;
    let rows = db.execute_query("SELECT 1").await.unwrap();
    assert_eq!(rows.len(), 1);
}
```

- **Timeouts** — wrap potentially hanging operations with `tokio::time::timeout`
- **Task cancellation** — test what happens when a spawned task is dropped mid-flight
- **Channel closure** — test behavior when sender or receiver is dropped

### Testing `Component` trait implementations

For TUI components with `handle_key`, `update`, `render`:

- Test `handle_key` by sending `KeyEvent` sequences and checking returned `Action` variants
- Test state changes after key sequences
- Don't test rendering pixel-by-pixel — verify the data backing the render is correct
- Test focus/blur transitions if the component behaves differently when focused

### Where to put tests

- **Unit tests**: Inline `#[cfg(test)] mod tests` in the same file — this is the standard convention
- **Integration tests**: `tests/` directory at crate root for cross-module interactions or public API surface testing
- **Test helpers**: If multiple test modules need the same setup, create a `#[cfg(test)]` helper function in the module or a shared test utility module

## Workflow

### Analyze mode

1. Run `cargo tarpaulin --workspace --timeout 120 --out stdout` for baseline
2. Parse output to identify low-coverage files
3. Read the low-coverage files and understand what's untested
4. Apply Edge Case Discovery categories to each untested area
5. Prioritize by risk:
   - **High**: Public API functions, error handling paths, data mutation logic, SQL generation
   - **Medium**: State transitions, boundary conditions, format/display
   - **Low**: Internal helpers with single callers, trivial getters
6. Present findings as a prioritized list with specific test suggestions

### Write mode

1. Read the target module — understand every function, branch, and error path
2. Run `cargo test` to confirm existing tests pass
3. Run `cargo tarpaulin -p <crate> --timeout 120 --out stdout` for baseline coverage
4. Apply Edge Case Discovery to identify what's missing
5. Write tests in the existing `#[cfg(test)] mod tests` block (create one if absent)
6. Run `cargo test` — all new tests must pass
7. Run `cargo fmt` and `cargo clippy --all-features --all-targets -- -D warnings`
8. Run coverage again and report the delta

### Combined mode

Analyze first, then fill the highest-priority gaps (or ask the user which gaps to fill).

## What NOT to do

- Don't add `pub` visibility just to make something testable — test through the public API or use `pub(crate)` test helpers
- Don't mock what you can construct — prefer real instances over mock objects when feasible
- Don't test private implementation details that may change — test observable behavior
- Don't write tautological tests that just duplicate the implementation logic
- Don't add test-only dependencies without discussing with the user first
- Don't ignore existing test patterns in the codebase — match the style that's already there
- Don't commit test database files or coverage output directories
