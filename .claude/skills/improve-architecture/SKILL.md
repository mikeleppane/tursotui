---
name: improve-architecture
description: Explore a codebase to find opportunities for architectural improvement, focusing on making the codebase more testable by deepening shallow modules. Rust-first but applicable to any language. Use when user wants to improve architecture, find refactoring opportunities, consolidate tightly-coupled modules, reduce coupling, simplify module boundaries, or make a codebase more testable and AI-navigable. Also use when the user mentions "shallow modules", "module deepening", "trait boundary design", or "architectural friction".
---

# Improve Codebase Architecture

Explore a codebase organically, surface architectural friction, discover opportunities for
improving testability, and propose module-deepening refactors as actionable plan documents.

A **deep module** (John Ousterhout, *A Philosophy of Software Design*) has a small interface
hiding a large implementation. Deep modules are more testable, more AI-navigable, and let you
test at the boundary instead of testing internals. The goal is to find shallow modules —
where the interface is nearly as complex as the implementation — and propose ways to deepen
them.

## Process

### 1. Explore the codebase

Use the Agent tool with `subagent_type=Explore` to navigate the codebase organically. Do NOT
follow rigid heuristics — explore like a developer encountering the code for the first time
and note where you experience friction:

- Where does understanding one concept require bouncing between many small files?
- Where are modules so shallow that the interface is nearly as complex as the implementation?
- Where have pure functions been extracted just for testability, but the real bugs hide in how
  they're called?
- Where do tightly-coupled modules create integration risk in the seams between them?
- Which parts of the codebase are untested, or hard to test?

**Rust-specific friction signals** — these are especially worth noting:

- **Visibility leaks**: `pub` items that should be `pub(crate)`, or `pub(crate)` on internal
  helpers that only one module uses — the visibility boundary doesn't match the conceptual
  boundary
- **Scattered impls**: A single type's `impl` blocks spread across multiple files, forcing
  you to search to understand what it can do
- **God enums**: An enum that every module imports and matches on — changes to it ripple
  everywhere. Often a sign that behavior should be dispatched via traits instead
- **Trait bloat**: Traits with many methods where most implementors only care about a few —
  the interface isn't pulling its weight as an abstraction
- **Orphan `mod.rs` re-exports**: A module directory whose `mod.rs` is just `pub mod` and
  `pub use` lines — the module boundary exists in the filesystem but not in the design
- **Stringly-typed interfaces**: `String` or `&str` parameters where a newtype or enum would
  prevent invalid states
- **Clone-heavy data flow**: Excessive `.clone()` calls often signal unclear ownership
  boundaries between modules — who owns this data?
- **Leaky async boundaries**: `async` functions that force callers to manage runtime details
  (spawning, channel setup) instead of hiding that complexity behind a synchronous-looking
  interface

The friction you encounter IS the signal.

### 2. Present candidates

Present a numbered list of deepening opportunities. For each candidate, show:

- **Cluster**: Which modules, types, and traits are involved
- **Why they're coupled**: Shared types, call patterns, co-ownership of a concept
- **Rust-specific signal**: Which friction signals from Step 1 apply (visibility leaks,
  scattered impls, god enums, etc.)
- **Dependency category**: See [REFERENCE.md](REFERENCE.md) for the four categories
- **Test impact**: What existing tests would be replaced by boundary tests, and what
  currently-untestable behavior would become testable

Do NOT propose interfaces yet. Ask the user: *"Which of these would you like to explore?"*

### 3. User picks a candidate

Wait for the user to choose. They may also ask clarifying questions or suggest combining
candidates — that's fine, adapt.

### 4. Frame the problem space

Before designing solutions, write a clear explanation of the problem space for the chosen
candidate:

- The constraints any new interface would need to satisfy
- The ownership and lifetime relationships involved
- The dependencies it would need to rely on
- A rough illustrative code sketch to make the constraints concrete — this is NOT a proposal,
  just a way to ground the discussion

Show this to the user, then immediately proceed to Step 5. The user reads and thinks while
the sub-agents work in parallel.

### 5. Design multiple interfaces

Spawn 3+ sub-agents in parallel using the Agent tool. Each must produce a **radically
different** interface for the deepened module.

Prompt each sub-agent with a separate technical brief: file paths, coupling details,
dependency category, what complexity is being hidden, and the Rust-specific constraints
(ownership, lifetimes, trait bounds). Give each agent a different design constraint:

- **Agent 1 — Minimal surface**: "Minimize the interface — aim for 1-3 entry points max.
  Hide everything behind a small set of methods."
- **Agent 2 — Flexible traits**: "Design around trait-based abstraction. Define trait
  boundaries that allow multiple implementations and make testing trivial via mock/stub
  impls."
- **Agent 3 — Caller-optimized**: "Optimize for the most common caller — make the default
  case trivial. Builder pattern, sensible defaults, progressive disclosure of complexity."
- **Agent 4** (if cross-boundary deps exist): "Design around ports & adapters — define a
  port trait at the module boundary, with production and test adapter implementations."

Each sub-agent outputs:

1. **Interface signature** — types, traits, methods, parameters with full Rust signatures
2. **Usage example** — how callers use it, showing ownership/borrowing at call sites
3. **What it hides** — what complexity is internal to the module
4. **Dependency strategy** — how deps are handled (see [REFERENCE.md](REFERENCE.md))
5. **Trade-offs** — what you gain and what you give up

Present designs sequentially, then compare them in prose.

After comparing, give your own recommendation: which design you think is strongest and why.
If elements from different designs combine well, propose a hybrid. Be opinionated — the user
wants a strong read, not just a menu.

### 6. User picks an interface (or accepts recommendation)

### 7. Create refactor plan

Write a refactor plan document at the path the user specifies (default: `docs/plans/`).
Use the template in [REFERENCE.md](REFERENCE.md).

The plan should be durable — it describes responsibilities, boundaries, and contracts rather
than being coupled to current file paths that will change during the refactor.
