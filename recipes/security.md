# Security Audit

Perform a security audit of this codebase. Use codemap to systematically find and analyze security-sensitive code.

## Step 1: Get a structural overview

Run `codemap map --tokens 3000` to understand the codebase layout and identify entry points (API routes, handlers, CLI commands, etc.).

## Step 2: Find security-sensitive areas

Run these context queries to locate relevant code:

- `codemap context "authentication authorization login session token" --include-content --limit 10`
- `codemap context "user input validation sanitize parse request body query params" --include-content --limit 10`
- `codemap context "database query SQL execute" --include-content --limit 10`
- `codemap context "file read write path upload download" --include-content --limit 10`
- `codemap context "secret key password env config credential" --include-content --limit 10`
- `codemap context "exec spawn shell command child process" --include-content --limit 10`
- `codemap context "redirect url href render html template" --include-content --limit 10`

## Step 3: Trace sensitive functions

For any security-critical function found (auth checks, input parsers, query builders, etc.), run `codemap symbol <name>` to find every call site. Verify that:
- Auth checks are applied consistently
- Input validation happens before use
- Sensitive data doesn't leak through logs or error messages

## Step 4: Check dependency flow

For files handling sensitive operations, run `codemap deps <file> --depth 2` to trace what data flows in and out. Look for paths where untrusted input reaches sensitive operations without validation.

## Step 5: Report vulnerabilities

For each finding, report:

- **File**: path and line reference
- **Severity**: critical / high / medium / low
- **Category**: injection, auth bypass, data exposure, insecure config, XSS, CSRF, path traversal, or other
- **Finding**: describe the vulnerability and how it could be exploited
- **Recommendation**: specific fix with code guidance

End with a summary table of all findings sorted by severity and a prioritized remediation plan.
