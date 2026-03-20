---
name: git-conventions
description: "Git commit conventions with enforced scopes, conventional commits, and meaningful
  commit messages. Use when creating git commits, writing commit messages, staging changes,
  or any git workflow in this project. Always follow these conventions for every commit."
---

# Git Commit Conventions

> Format: Conventional Commits | Scopes: Required | Breaking changes: `!` suffix + footer

---

## Commit Message Format

Every commit message follows this structure:

```
<type>(<scope>): <subject>

<body>

<footer>
```

**All three parts matter.** The subject line gets people's attention, the body tells the
story, the footer records breaking changes and references.

---

## Subject Line

```
feat(editor): add multi-cursor support for batch edits
```

- **Max 72 characters** — truncated in `git log --oneline` and GitHub UI otherwise
- **Lowercase** — no capital first letter after the colon
- **Imperative mood** — "add" not "added" or "adds". Read it as: "this commit will _add
  multi-cursor support_"
- **No trailing period**
- **Be specific** — describe *what changed*, not *what you did*. "fix query timeout" not
  "fix bug" or "update code"

---

## Types

Use exactly these types — no others:

| Type | When to use |
|------|------------|
| `feat` | New feature or capability for the user |
| `fix` | Bug fix — something was broken, now it works |
| `refactor` | Code restructuring with no behavior change |
| `perf` | Performance improvement with no behavior change |
| `docs` | Documentation only — README, doc comments, specs, plans |
| `test` | Adding or updating tests with no production code change |
| `build` | Build system, dependencies, Cargo.toml changes |
| `ci` | CI/CD pipeline configuration |
| `chore` | Maintenance that doesn't fit above — formatting, linting, tooling config |

**Choosing the right type:**
- Changed behavior? → `feat` or `fix`
- Same behavior, different structure? → `refactor`
- Same behavior, faster? → `perf`
- Only tests changed? → `test`
- Only docs changed? → `docs`

---

## Scopes (Required)

Every commit must include a scope that identifies the area of the codebase affected.
Scopes should be short, lowercase, and consistent.

**Application scopes:**

| Scope | Area |
|-------|------|
| `app` | Application state, event loop, top-level orchestration |
| `db` | Database connection, query execution, channels |
| `schema` | Schema tree panel, table/column/index loading |
| `editor` | SQL editor panel, syntax highlighting, buffer management |
| `results` | Results panel, table rendering, column sizing |
| `detail` | Record detail panel, row inspection |
| `explain` | Query explain/analysis panel |
| `er` | ER diagram panel |
| `pragmas` | Pragma viewer/editor panel |
| `dbinfo` | Database info panel |
| `data-editor` | Inline data editing, DML generation, staged changes |
| `history` | Query history storage and search |
| `theme` | Theming, colors, styling |
| `config` | Configuration loading, defaults, file I/O |
| `cli` | CLI argument parsing, entry point |
| `nav` | Navigation, focus management, keybindings |
| `clipboard` | Clipboard integration |
| `ui` | Shared UI components, layout, widgets |

**Infrastructure scopes:**

| Scope | Area |
|-------|------|
| `deps` | Dependency additions, removals, or upgrades |
| `ci` | CI/CD pipeline |
| `release` | Version bumps, changelogs, release prep |
| `tooling` | Dev tooling, linting config, formatters |

When a change spans multiple areas, use the scope of the *primary* change. If genuinely
cross-cutting, use `app`.

Scopes will evolve as the project grows — add new ones when a new module or area emerges
that doesn't fit existing scopes. Keep them short and descriptive.

---

## Body

The body explains **why** this change exists, not what it does (the diff shows that).

```
feat(results): add horizontal scrolling for wide result sets

Tables with many columns were clipped at the terminal edge, making it
impossible to inspect columns beyond the visible width. Users had to
resize their terminal or reduce font size as a workaround.

Track a horizontal scroll offset per result set and shift the visible
column window with left/right arrow keys. Column headers scroll in
sync with data rows.
```

**Body guidelines:**
- **Wrap at 72 characters** — respects `git log` formatting in terminals
- **Blank line** between subject and body
- **First paragraph: the problem or motivation.** What was wrong, missing, or needed?
  Why does this change exist?
- **Second paragraph (optional): the approach.** How does this change solve the problem?
  Only include when the approach isn't obvious from the diff — architectural decisions,
  tradeoffs made, alternatives considered and rejected.
- **Skip the body** only for truly trivial changes: typo fixes, import cleanup, formatting.
  If you're tempted to skip it, the commit might be too small or you might be underestimating
  the future reader's need for context.

---

## Breaking Changes

For changes that break the public API or change user-facing behavior in incompatible ways:

1. **Add `!` after the scope** in the subject line
2. **Add a `BREAKING CHANGE:` footer** explaining what broke and how to migrate

```
feat(config)!: change config file format from JSON to TOML

The configuration file format has changed to TOML for better readability
and comment support.

BREAKING CHANGE: config.json is no longer read. Rename to config.toml
and convert the syntax. See docs/migration.md for a conversion guide.
```

Both the `!` marker and the footer are required — the `!` for quick scanning in git log,
the footer for migration details.

---

## What Makes a Good Commit

### Atomic commits
Each commit should be a single logical change that compiles and passes tests on its own.
If you're writing "and" in your subject line, consider splitting.

**Good — one concern per commit:**
```
refactor(db): extract connection pool into dedicated module
feat(db): add connection retry with exponential backoff
test(db): add connection pool timeout tests
```

**Bad — multiple concerns bundled:**
```
feat(db): refactor connection pool and add retry logic and tests
```

### Commit the why, not the what
The diff shows exactly what changed. The commit message should answer: "Six months from
now, when someone reads `git blame` on this line, what context will they need?"

---

## Things to Avoid

- **Vague messages:** "fix bug", "update code", "changes", "WIP", "misc"
- **Referencing conversations:** "as discussed", "per review feedback", "Claude suggested"
- **Temporal language:** "now we do X", "previously this was Y"
- **Implementation narration:** "first I changed X, then I updated Y"
- **Emoji** in commit messages
- **Ticket/issue numbers in the subject** — put them in the footer if needed:
  `Refs: #42`

---

## Footer

Footers go after the body, separated by a blank line. Use them for:

```
BREAKING CHANGE: description of what broke and migration path
Refs: #123, #456
Closes: #789
```

Only include footers when they carry information. Don't add empty footers.

---

## Commit Workflow

When creating a commit:

1. **Review staged changes** — make sure the diff matches a single logical change
2. **Choose the right type** — use the type table above
3. **Pick the scope** — which area of the codebase is primarily affected?
4. **Write a specific subject** — what does this commit do, in imperative mood?
5. **Write the body** — why does this change exist? What problem does it solve?
6. **Check for breaking changes** — does this change user-facing behavior incompatibly?
7. **Verify the commit compiles** — never commit code that doesn't build
