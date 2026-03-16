# Test Generation

Identify the most important untested code in this codebase and generate tests for it. Use codemap to prioritize what to test based on structural importance.

## Step 1: Understand what exists

Run `codemap map --tokens 3000` to see the highest-ranked files and their exports.

Run `codemap context "test spec" --limit 10` to find existing test files and understand current test patterns, conventions, and frameworks in use.

## Step 2: Identify untested code

Compare the top-ranked files from the map against existing test coverage. Focus on files that:
- Rank highly (many dependents) but have no corresponding test file
- Contain business logic, data transformations, or validation
- Are imported by many other files (breakage has wide impact)

## Step 3: Understand what to test

For each file you plan to test:

1. Run `codemap deps <file>` to see what it depends on (these may need mocking) and what depends on it (these define the contract to verify)
2. Run `codemap symbol <exported-function>` for key exports to understand how they're called across the codebase — real usage patterns inform good test cases
3. Read the file content with `codemap context "<file's purpose>" --include-content` to understand the implementation

## Step 4: Generate tests

For each file, write tests that cover:

- **Happy path**: normal inputs produce expected outputs
- **Edge cases**: empty inputs, boundary values, null/undefined
- **Error cases**: invalid inputs, missing dependencies, failure modes
- **Integration points**: verify the function works correctly with its real dependencies where practical

Follow the existing test conventions found in Step 1 (framework, file naming, directory structure, assertion style).

## Step 5: Summary

After generating tests, provide:

- List of new test files created
- What each test covers and why it was prioritized
- Remaining untested areas and suggested next steps
