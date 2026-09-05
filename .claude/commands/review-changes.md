Review all staged and unstaged changes in the working tree.

Use the [canonical CI gate list](/CLAUDE.md#ci-gates) as the sole source for
required verification. Do not copy its commands into this file.

1. Run `git diff` and `git diff --cached` to see all changes.
2. Review each changed file for:
   - Correctness and logic errors
   - Style violations (see CLAUDE.md conventions)
   - Missing error handling
   - Security concerns (hardcoded secrets, injection risks)
   - Missing or incorrect tests
3. Suggest specific improvements with code examples.
4. Rate the overall change quality: Good / Needs Work / Concerns.
