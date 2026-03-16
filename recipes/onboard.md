# Codebase Onboarding

Generate a comprehensive onboarding guide for this codebase. Use codemap to build a structural understanding and explain how everything connects.

## Step 1: Get the big picture

Run `codemap map --tokens 5000` to get the full structural overview. This shows the most important files ranked by their centrality in the dependency graph, along with their key exports.

## Step 2: Identify the architecture

From the map output, identify:
- **Entry points**: CLI commands, API routes, main files, or handler registrations
- **Core modules**: the highest-ranked files that everything depends on
- **Data layer**: database access, API clients, file I/O
- **Shared types**: type definitions, interfaces, constants used across the codebase

## Step 3: Trace the data flow

Pick the 2-3 most important entry points and trace how data flows through the system:

1. Run `codemap deps <entry-point-file>` to see what it imports
2. Follow the chain: run `codemap deps <imported-file>` for key dependencies
3. Run `codemap symbol <key-function>` for critical functions to see where they're defined and used

Build a mental model of: input comes in here → gets processed by these modules → state is stored/transformed here → output goes here.

## Step 4: Understand key abstractions

For the most important shared types and interfaces, run `codemap symbol <type-name>` to see where they're defined and every file that references them. This reveals the contracts that hold the codebase together.

## Step 5: Write the onboarding guide

Produce a guide structured as:

1. **What this project does** — one paragraph summary
2. **Tech stack** — languages, frameworks, key libraries
3. **Project structure** — directory layout and what lives where
4. **Architecture overview** — how the main pieces connect (include an ASCII diagram if helpful)
5. **Key data flows** — walk through 2-3 primary operations end to end
6. **Important abstractions** — the core types/interfaces/patterns a new developer must understand
7. **Development workflow** — how to build, test, and run locally (infer from package.json, Makefile, etc.)
8. **Where to start** — recommended first files to read, in order
