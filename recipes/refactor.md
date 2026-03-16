# Refactoring Opportunities

Analyze this codebase for refactoring opportunities. Use codemap to identify complexity hotspots and tightly coupled areas, then suggest concrete improvements.

## Step 1: Find complexity hotspots

Run `codemap map --tokens 5000` to see the highest-ranked files. Files at the top have the most connections in the dependency graph — they are the most important and often the most complex. Focus your analysis on the top 10-15 files.

## Step 2: Analyze coupling

For each high-ranked file, run `codemap deps <file>` and note:
- **High fan-out** (many imports): the file may be doing too much and could be split
- **High fan-in** (many importers): changes to this file ripple widely — its API should be stable and minimal
- **Mutual dependencies**: files that import each other signal tangled responsibilities

Run `codemap deps <file> --depth 2` on suspicious files to see if clusters of tightly coupled files emerge.

## Step 3: Examine large files

For files that appear to have many exports in the map output, run `codemap context "<file's apparent responsibility>" --include-content` and read the code. Look for:
- Functions longer than ~50 lines
- Multiple unrelated responsibilities in one file
- Duplicated logic across files
- Deeply nested conditionals or callbacks
- God objects or utility grab-bags

## Step 4: Check symbol usage

For exports that seem overly broad or poorly named, run `codemap symbol <name>` to see how they're actually used. If a function is only used in one place, it may not need to be exported. If it's used everywhere, it may need a clearer contract.

## Step 5: Propose refactorings

For each opportunity, provide:

- **Location**: file path(s) involved
- **Problem**: what makes this code hard to maintain (be specific)
- **Proposed refactoring**: concrete steps (extract module, split file, introduce interface, inline abstraction, etc.)
- **Impact**: what improves — readability, testability, change isolation, etc.
- **Risk**: what could break and how to mitigate it

Prioritize refactorings by impact-to-effort ratio. End with a suggested order of operations (what to refactor first to unblock later improvements).
