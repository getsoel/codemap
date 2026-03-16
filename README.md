# codemap

Code intelligence for JS/TS codebases. Parses your project, builds a dependency graph, ranks files by structural importance (PageRank), and gives Claude Code a map of your codebase on every session start.

## Install

```bash
npm install @soel/codemap
```

Or build from source:

```bash
cargo build --release
```

## Setup

One command indexes your project and configures Claude Code hooks:

```bash
codemap setup
```

This will:
1. Index all JS/TS files in the project
2. Configure a **SessionStart** hook to inject the code map into Claude's context
3. Configure a **PostToolUse** hook to re-index incrementally after file edits

Options:
- `--global` — write to `~/.claude/settings.json` instead of project-local
- `--no-post-hook` — skip the PostToolUse re-indexing hook
- `--dry-run` — preview the config without writing

## Commands

### `codemap index`

Parse files, build the dependency graph, and compute PageRank.

```bash
codemap index              # full index
codemap index --force      # ignore cache, re-parse everything
codemap index --incremental  # only re-parse files with newer mtime
```

### `codemap map`

Print a ranked code map showing the most structurally important files and their exported signatures.

```bash
codemap map                # default ~1500 token budget
codemap map --tokens 3000  # larger budget
codemap map --no-instructions  # omit the CLI hints footer
```

Example output:

```
## Codebase Map (codemap v0.2.0)
Indexed 142 files | 847 exports | 312 import edges
Top files by structural importance (PageRank):

src/services/auth.ts [rank: 0.94 | 12 importers]
  export class AuthService
    constructor(private db: Database, private jwt: JwtService)
    async login(email: string, password: string): Promise<AuthResult>
  → imports: src/db/users.ts, src/utils/crypto.ts

src/models/user.ts [rank: 0.82 | 23 importers]
  export interface User
  export interface CreateUserInput
  export type UserRole = "admin" | "member" | "guest"
```

### `codemap symbol <pattern>`

Find where a symbol is defined and who uses it.

```bash
codemap symbol useAuth           # substring match
codemap symbol useAuth --exact   # exact match only
codemap symbol useAuth --all     # show all references (no truncation)
codemap symbol useAuth --json    # structured output
codemap symbol useAuth --limit 5
```

### `codemap deps <file>`

Show the import graph neighborhood of a file.

```bash
codemap deps src/services/auth.ts
codemap deps src/services/auth.ts --direction imports    # what it imports
codemap deps src/services/auth.ts --direction importers  # who imports it
codemap deps src/services/auth.ts --depth 2              # 2-hop traversal
codemap deps src/services/auth.ts --all --json
```

### `codemap context "<query>"`

Suggest the most relevant files for a natural language task.

```bash
codemap context "add OAuth Google login"
codemap context "add OAuth Google login" --include-content  # include file contents
codemap context "fix auth bug" --limit 5 --json
```

## Global flags

```
-r, --root <PATH>   Project root directory (default: .)
-v                   Verbosity: -v (INFO), -vv (DEBUG), -vvv (TRACE)
```

## How it works

1. **Discover** JS/TS files using the [ignore](https://crates.io/crates/ignore) crate (respects `.gitignore` and `.codemapignore`)
2. **Parse** with [oxc](https://oxc.rs) — extract imports, exports, and top-level symbols
3. **Resolve** import paths with [oxc_resolver](https://crates.io/crates/oxc_resolver) (handles tsconfig paths, package.json exports, .js→.ts mapping)
4. **Build** a directed dependency graph with [petgraph](https://crates.io/crates/petgraph)
5. **Rank** files using PageRank (damping 0.85, 100 iterations)
6. **Store** everything in SQLite (`.codemap/index.db`) for incremental updates

Re-indexing after a single file edit typically completes in under 500ms.

## Configuration

### `.codemapignore`

Standard gitignore syntax. Excludes files from indexing (higher precedence than `.gitignore`):

```
dist/
build/
*.min.js
__tests__/
```

### `.codemap/`

Generated directory containing `index.db` (the SQLite database). Add to `.gitignore`.

## Claude Code integration

codemap integrates with Claude Code through [hooks](https://docs.anthropic.com/en/docs/claude-code/hooks):

- **SessionStart** — `codemap map` runs on every session start, injecting a ranked code map into Claude's context so it knows the shape of your codebase before you ask anything
- **PostToolUse** — `codemap index --incremental` runs asynchronously after Write/Edit operations, keeping the index fresh

Run `codemap setup` to configure these automatically.

## License

MIT
