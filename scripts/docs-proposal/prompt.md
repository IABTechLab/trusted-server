# Documentation proposal

You are updating the public Trusted Server documentation after a commit merged
to `main`. The merged commit, its base, and the table mapping implementation
sources to documentation pages appear at the end of this file.

## Task

1. Inspect the merged change from its base to its commit with
   `git log --oneline <base>..<commit>`, `git diff --stat <base> <commit>`,
   and `git diff <base> <commit>`. The range covers every commit the push
   added; `git show` omits the changes of a clean merge commit.
2. Decide whether it changes behavior that `docs/index.md` or a page under
   `docs/guide/` describes: configuration fields and defaults, routes, adapter
   support, integrations, auction behavior, CLI commands and flags, browser
   bundles, or verification commands. Use the Sources of truth table to find
   the affected pages.
3. If no reader-facing documentation is affected, change nothing and write
   `.docs-proposal/rationale.md` containing one sentence explaining why.
4. Otherwise edit only the affected pages, then write
   `.docs-proposal/rationale.md` with one bullet per edited page naming the
   page, what changed, and the merged source file that justifies it.

## Rules

- Describe behavior the merged code ships. Do not document intent, roadmap
  items, or unmerged work.
- Read the merged source before writing. Every statement you add must be
  verifiable in the code at this commit.
- Make the smallest edit that keeps the page accurate. Do not restyle,
  reorder, or rewrite unaffected text.
- Use only fictional values: `example.com` domains and placeholder secrets.
  Never copy real domains, customer names, credentials, or service IDs.
- Keep existing links and anchors working, and link to other pages with
  relative paths.
- Do not edit files outside `docs/guide/`, `docs/index.md`, and
  `.docs-proposal/`. Do not commit, push, or create branches.
