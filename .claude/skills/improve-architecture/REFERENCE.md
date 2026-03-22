# Reference

## Dependency Categories

When assessing a candidate for deepening, classify its dependencies into one of these four
categories. The category determines the testing and abstraction strategy.

### 1. In-process

Pure computation, in-memory state, no I/O. Always deepenable — merge the modules and test
directly.

**Rust example**: Two modules that share types and call each other's functions but never touch
the filesystem, network, or database. Merge them, make the shared types private to the new
module, and test through the public interface.

### 2. Local-substitutable

Dependencies that have local test stand-ins (e.g., SQLite for a database abstraction,
`tempdir` for filesystem operations, in-memory channels for network protocols). Deepenable
if the test substitute exists.

**Rust example**: A module that wraps database queries. The deepened module is tested with an
in-memory SQLite database — no mocks needed, real queries run in tests.

### 3. Remote but owned (Ports & Adapters)

Your own services across a boundary you control (other crates in the workspace, internal
APIs, separate processes). Define a port (trait) at the module boundary. The deep module owns
the logic; the transport is injected.

**Rust example**:
```rust
// The port — a trait defining what the module needs
pub(crate) trait StoragePort {
    fn save(&self, key: &str, value: &[u8]) -> Result<()>;
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>>;
}

// Production adapter
struct FileStorage { root: PathBuf }
impl StoragePort for FileStorage { /* ... */ }

// Test adapter
#[cfg(test)]
struct InMemoryStorage { data: HashMap<String, Vec<u8>> }
#[cfg(test)]
impl StoragePort for InMemoryStorage { /* ... */ }
```

### 4. True external (Mock at boundary)

Third-party services or system resources you don't control. Mock at the boundary. The
deepened module takes the external dependency as an injected trait, and tests provide a mock
or stub implementation.

**Rust example**: A module that calls an external HTTP API. Define a trait for the API
surface, implement it with `reqwest` in production, and provide a stub in tests that returns
canned responses.

---

## Rust-Specific Deepening Patterns

These patterns come up repeatedly when deepening Rust modules.

### Trait as boundary

The most common Rust deepening pattern. Define a trait that represents the module's public
contract, then hide the implementation behind it. Callers depend on the trait, not the
concrete type. This makes the module testable (swap in a test impl) and extensible (add new
impls without changing callers).

When the trait has only one production implementation, consider whether a concrete type with
`pub(crate)` methods is simpler. Traits earn their keep when they enable testing or when
multiple implementations are genuinely needed.

### Newtype wrappers for domain boundaries

When two modules pass raw primitives (`String`, `u64`, `Vec<u8>`) between them, introduce
newtypes at the module boundary. This makes the contract explicit and prevents cross-module
misuse without runtime cost.

### Builder pattern for complex construction

When a deepened module has many configuration options, expose a builder rather than a
constructor with many parameters. The builder is the small interface; the configuration
complexity is hidden inside.

### Channel-based async boundary

For async modules, consider making the boundary a channel pair (`mpsc::Sender` /
`mpsc::Receiver`) rather than a trait with async methods. The caller sends requests and
receives responses — the module's internal async machinery (spawned tasks, timers, retry
logic) is completely hidden. This avoids `async_trait` overhead and makes the boundary
trivially testable.

### Visibility as encapsulation

Rust's visibility system (`pub`, `pub(crate)`, `pub(super)`, private) IS the encapsulation
mechanism. When deepening a module:

- The module's public trait or API methods are `pub(crate)`
- Internal types, helpers, and implementation details are private
- If something was `pub` only because it was in a separate file, making it private after
  merging is the whole point

---

## Testing Strategy

The core principle: **replace, don't layer.**

- Old unit tests on shallow modules become redundant once boundary tests exist — delete them
- Write new tests at the deepened module's interface boundary
- Tests assert on observable outcomes through the public interface, not internal state
- Tests should survive internal refactors — they describe behavior, not implementation
- In Rust, `#[cfg(test)] mod tests` in the deepened module file is the natural home for
  boundary tests

### What good boundary tests look like in Rust

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_handles_empty_input() {
        let module = DeepModule::new(/* test deps */);
        let result = module.process(&[]);
        assert_eq!(result, Expected::Empty);
    }

    #[test]
    fn module_rejects_invalid_state() {
        let module = DeepModule::new(/* test deps */);
        let err = module.process(&invalid_input()).unwrap_err();
        assert!(matches!(err, ModuleError::InvalidInput { .. }));
    }
}
```

Tests call the public interface, assert on outputs and errors, and don't reach into private
fields. If internal restructuring breaks these tests, the module's contract changed — and
that's worth knowing.

---

## Plan Document Template

Use this template when writing the refactor plan in Step 7.

```markdown
# Refactor: [Title]

## Problem

Describe the architectural friction:

- Which modules are shallow and tightly coupled
- What integration risk exists in the seams between them
- Why this makes the codebase harder to navigate, test, and maintain
- Which Rust-specific friction signals are present

## Proposed Interface

The chosen interface design:

- Interface signature (traits, types, methods, parameters — full Rust signatures)
- Usage example showing how callers use it, including ownership/borrowing
- What complexity it hides internally

## Dependency Strategy

Which category applies and how dependencies are handled:

- **In-process**: merged directly, shared types made private
- **Local-substitutable**: tested with [specific stand-in]
- **Ports & adapters**: trait definition, production impl, test impl
- **Mock**: trait boundary for external services

## Testing Strategy

- **New boundary tests to write**: behaviors to verify at the interface
- **Old tests to delete**: shallow module tests that become redundant
- **Test environment needs**: any local stand-ins or adapters required

## Implementation Steps

Ordered steps for executing the refactor:

1. Step description — what changes, what moves, what gets deleted
2. ...

Each step should be independently compilable (`cargo check` passes after each step).

## Architectural Guidance

Durable guidance that is NOT coupled to current file paths:

- What the module should own (responsibilities)
- What it should hide (implementation details)
- What it should expose (the interface contract)
- How callers should migrate to the new interface
```
