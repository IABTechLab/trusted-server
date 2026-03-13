Review all staged and unstaged changes in the working tree.

1. Run `git diff` and `git diff --cached` to see all changes.
2. Review each changed file for:
   - Correctness and logic errors
   - Style violations (see CLAUDE.md conventions)
   - Missing error handling
   - Security concerns (hardcoded secrets, injection risks)
   - Missing or incorrect tests
3. Suggest specific improvements with code examples.
4. Rate the overall change quality: Good / Needs Work / Concerns.
