---
name: rust-standards
description: "Rust coding standards with ownership patterns, type safety, clean code, and
  performance-conscious design. Use when creating or editing Rust files (*.rs), reviewing
  Rust code, or making any decisions in Rust projects. This skill applies to ALL Rust
  development — new files, modifications, refactoring, debugging, or code review."
---

# Rust Coding Standards

> Target: Latest stable Rust | Edition: 2024 | Linting: `cargo clippy -- -D warnings --all-features --all-targets` | Formatting: `cargo fmt`

---

## Core Philosophy

Write code that is correct first, clear second, and fast third. Leverage Rust's type system
and ownership model to catch bugs at compile time rather than runtime. The compiler is your
most powerful reviewer — design code so it rejects invalid states before they exist.

---

## Ownership & Borrowing

The borrow checker enforces memory safety without a garbage collector. Work with it, not
around it.

- **Borrow before clone.** If a function only reads data, take `&T` not `T`. Every `.clone()`
  is a code smell — justify each one.
- **Own when storing.** Structs that persist data should own it. Reach for lifetimes only
  when the borrowed data provably outlives the struct.
- **`&str` over `&String`** in parameters — accept the more general type.
- **`Cow<'_, str>`** when a function sometimes allocates and sometimes doesn't.
- **Return owned types** from public APIs unless there's a clear performance reason for
  references.

---

## Type System

Rust's type system is expressive enough to encode most invariants at compile time. Use it.

- **Make illegal states unrepresentable.** Encode invariants in types so invalid
  configurations don't compile. A `Connection<Authenticated>` that can only be constructed
  after login is stronger than a runtime `is_authenticated` check.
- **Enums over strings or sentinel values.** `Status::Active` is safer and more
  self-documenting than `"active"`. Strings are for human-facing text, not program state.
- **Newtypes for domain concepts.** `struct UserId(u64)` prevents accidentally passing a
  `PostId` where a `UserId` is expected — a class of bug that's invisible with raw integers.
- **Exhaustive pattern matching.** Avoid catch-all `_ =>` arms when matching enums. Listing
  each variant explicitly means the compiler flags new additions as errors, forcing you to
  handle them.
- **Avoid boolean parameters.** `render(true)` at the call site is opaque. Use a two-variant
  enum: `render(Visible::Yes)` reads immediately.

---

## Error Handling

- **`Result<T, E>` for recoverable errors.** Never panic for expected failure modes.
- **`?` for propagation.** Don't manually match on `Result` just to re-wrap — the `?`
  operator handles conversion via `From`.
- **`thiserror`** for library/crate-internal error types where callers need to match variants.
- **`anyhow::Result`** for application-level code where callers just need the error message.
- **Avoid `unwrap()` and `expect()` in production paths.** Reserve them for cases where the
  invariant is provably guaranteed, and add a comment explaining why.
- **`Option` for absence, `Result` for failure.** If an error message would be useful,
  use `Result`.

---

## Pattern Matching & Control Flow

- **Exhaustive matching over catch-all.** List all enum variants explicitly. The compiler
  becomes your changelog reviewer.
- **`if let` for single-variant checks.** A full `match` for one arm is noise.
- **`matches!()` for boolean pattern checks:**
  `matches!(value, Pattern::A | Pattern::B)`
- **Early returns for guard clauses.** Reduce nesting by returning early on error/edge
  conditions.
- **`let-else` for conditional binding with early exit:**

  ```rust
  let Some(user) = users.get(id) else {
      return Err(Error::NotFound(id));
  };
  ```

---

## Iterators & Functional Patterns

- **Iterators over index loops.** `.iter().filter().map().collect()` over
  `for i in 0..vec.len()`.
- **Use adaptor methods** — `filter_map`, `flat_map`, `take_while`, `chain`, `zip`,
  `enumerate` — instead of manual accumulation with mutable state.
- **Lazy by default.** Chain adaptors without collecting intermediate results. Only
  `.collect()` at the end.
- **Turbofish when the target type isn't obvious:**
  `let ids: Vec<_> = items.iter().map(|i| i.id).collect();`
- **Avoid `.clone()` in chains.** Cloning inside `.map()` often signals a data flow problem.
- **`Iterator::by_ref()`** when you need to partially consume an iterator and continue later.

---

## Memory & Performance

- **Stack over heap.** Arrays and tuples over `Vec` and `Box` when size is known at
  compile time.
- **Minimize allocations.** Reuse buffers — `String::clear()` then reuse vs allocating anew.
- **`&[T]` over `&Vec<T>`** in function signatures — accept slices.
- **`Box<str>` over `String`** for immutable owned strings that won't be mutated or grown.
- **Avoid unnecessary `Arc`/`Rc`.** Transfer ownership if possible. Shared ownership is a
  last resort.
- **CPU-friendly data layout.** Prefer struct-of-arrays for columnar access patterns in hot
  loops. Keep hot fields adjacent for cache locality.
- **Pre-allocate with capacity.** `Vec::with_capacity(n)` when you know the size upfront
  avoids repeated reallocations.

---

## API Design

- **Accept generic inputs, return concrete types.**
  - Parameters: `impl Into<String>`, `impl AsRef<Path>`, `&str`
  - Returns: `String`, `PathBuf`, `Vec<T>` — concrete and predictable
- **Builder pattern** for structs with many optional fields. Avoid constructors with more
  than 3-4 parameters.
- **`#[must_use]`** on functions where ignoring the return value is almost certainly a bug.
- **Private by default.** Only `pub` what's part of the API. Use `pub(crate)` for internal
  sharing between modules.

---

## Naming Conventions

Follow Rust conventions without exception:

| Type | Convention | Example |
|------|------------|---------|
| Types, traits, enum variants | `PascalCase` | `AppState`, `Component`, `Action::FocusPanel` |
| Functions, methods, variables, modules | `snake_case` | `handle_key`, `panel_id` |
| Constants, statics | `SCREAMING_SNAKE_CASE` | `MAX_RETRIES`, `DARK_THEME` |
| Lifetimes | Short lowercase | `'a`, `'ctx` |
| Type parameters | Single uppercase or descriptive | `T`, `E`, `Item` |

**Semantic naming:**
- **Conversions:** `as_*` (cheap, borrowed -> borrowed), `to_*` (expensive, borrowed -> owned),
  `into_*` (consuming, owned -> owned)
- **Accessors:** `field()` not `get_field()` — Rust convention omits `get_`
- **Boolean methods:** `is_*`, `has_*`, `can_*`
- **Constructors:** `new()`, `with_*()`, `from_*()`, or `Default::default()`

---

## Struct & Enum Design

- **Derive order:** `Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default,
  Serialize, Deserialize` — only derive what's needed.
- **`#[non_exhaustive]`** on public enums and structs that may gain variants/fields.
- **Tuple structs for newtypes:** `struct Meters(f64);`
- **Unit variants for stateless options:** `enum Direction { Up, Down, Left, Right }`

---

## Async Patterns

- **Don't hold locks across `.await`.** Release `MutexGuard` before awaiting — otherwise
  you block the executor.
- **`tokio::spawn` for independent concurrent work.** Don't spawn for sequential operations.
- **Channels over shared state** for cross-task communication — `mpsc`, `oneshot`, `watch`.
- **`Send + 'static`** on spawned futures — structure data to satisfy this naturally rather
  than fighting it.
- **Fresh connections per task** when using connection pools — cloning a connection often
  shares internal state.

---

## Comments & Documentation

**Do:**
- `///` doc comments on all public items: functions, structs, enums, variants, trait methods
- Brief `//!` module-level docs explaining the module's purpose
- Document **why**, not what — the code shows what, comments explain reasoning
- `# Examples` in doc comments for non-obvious APIs
- `// SAFETY:` on every `unsafe` block explaining why invariants hold
- `// TODO:` with description of what's needed

**Don't:**
- Comments that restate the code: `// increment counter` above `counter += 1`
- References to AI conversations or generation context
- Temporal markers: "added", "new", "existing code", "Phase 1"
- Commented-out code — delete it, git remembers

---

## Code Organization

- **One concept per module.** Each module should have a single clear purpose.
- **`module_name.rs` over `module_name/mod.rs`** for leaf modules. Use directories only
  when a module has submodules.
- **Re-export public API** from parent modules so users don't need deep paths.
- **Tests in the same file** under `#[cfg(test)] mod tests`. Integration tests in `tests/`.

---

## Testing

- **Test behavior, not implementation.** Verify what the code does, not how.
- **Descriptive test names:** `test_empty_input_returns_none` over `test_basic`.
- **One assertion per concept.** Multiple asserts are fine if they verify the same logical
  thing.
- **`#[should_panic(expected = "...")]`** for panic tests — include the expected message.
- **Avoid test-only public methods.** Use `#[cfg(test)]` modules for test helpers.

---

## Unsafe Code

- **Avoid unless absolutely necessary.** Prefer safe abstractions.
- **Minimize scope.** Wrap the smallest possible block.
- **Every `unsafe` block needs `// SAFETY:`** explaining why invariants are upheld.
- **Encapsulate behind safe APIs.** Callers should never need `unsafe`.

---

## Avoid Over-Engineering

- Only make changes directly requested or clearly necessary
- Don't add features, refactoring, or "improvements" beyond what was asked
- Don't add docstrings or comments to code you didn't change
- Don't add error handling for scenarios that can't occur — trust internal invariants
- Don't create helpers or abstractions for one-time operations
- Three similar lines of code is better than a premature abstraction
- Don't design for hypothetical future requirements

---

## Cleanup

- **Delete unused code completely.** Don't comment it out, don't `_`-prefix, don't keep
  "just in case."
- **No backwards-compat hacks** — no renamed `_vars`, no re-exports of removed items,
  no `// removed` comments.
- **Remove dead `cfg` blocks** and feature flags that are no longer relevant.
- **Clean up imports** — no unused `use` statements. `cargo clippy` catches these.
