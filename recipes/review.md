# Code Review for Maintainability and Scalability

Review this codebase for maintainability and scalability issues. Use codemap to gather structural context, then analyze the results.

## Step 1: Understand the structure

Run `codemap map --tokens 3000` to get a ranked overview of the most important files and their exports.

## Step 2: Identify the area to review

Run `codemap context "<describe the feature or area to review>" --include-content --limit 10` to find the most relevant files. Read through the returned content carefully.

## Step 3: Check coupling

For each key file identified, run `codemap deps <file>` to see its imports and importers. Look for:
- Files with an unusually high number of importers (high fan-in — fragile to change)
- Files that import many others (high fan-out — doing too much)
- Circular or tightly coupled dependency clusters

## Step 4: Trace critical symbols

For any function or export that looks problematic, run `codemap symbol <name>` to find all definition sites and references. Check whether the API surface is clean and well-bounded.

## Step 5: Produce findings

Report your findings as a structured list. For each issue:

- **File**: path and line reference
- **Severity**: high / medium / low
- **Category**: coupling, complexity, naming, abstraction, error handling, or other
- **Finding**: what the problem is
- **Suggestion**: concrete fix or refactoring step

Sort findings by severity (high first). End with a summary of the codebase's overall maintainability and the top 3 actions that would have the most impact.
