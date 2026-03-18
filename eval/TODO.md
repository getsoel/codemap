# Eval improvements (deferred)

## Multiple runs per case

Run each case 2-3x to account for LLM non-determinism. Report median per case and flag unstable results where outcomes flip between runs. Even 2 runs lets you detect noise.

## Graph-derived hard cases

Add 5-10 cases per repo where expected files are non-obvious — structurally important but only reachable through 2-3 import hops. Use codemap's own dependency graph to find these. This tests the core value proposition: cases where keyword search fails but graph context helps.
