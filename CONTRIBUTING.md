# Contributing Guidelines

### Contents

- [Submitting Pull Requests](#repeat-submitting-pull-requests)
- [Writing Commit Messages](#memo-writing-commit-messages)
- [Code Review](#white_check_mark-code-review)
- [Coding Style](#nail_care-coding-style)
- [Credits](#pray-credits)

## :repeat: Submitting Pull Requests

We **love** pull requests! Before [forking the repo](https://help.github.com/en/github/getting-started-with-github/fork-a-repo) and [creating a pull request](https://help.github.com/en/github/collaborating-with-issues-and-pull-requests/proposing-changes-to-your-work-with-pull-requests) for non-trivial changes, it is usually best to first open an issue to discuss the changes, or discuss your intended approach for solving the problem in the comments for an existing issue.

For most contributions, after your first pull request is accepted and merged, you will be [invited to the project](https://help.github.com/en/github/setting-up-and-managing-your-github-user-account/inviting-collaborators-to-a-personal-repository) and given **push access**. :tada:

*Note: All contributions will be licensed under the project's license.*

- **Smaller is better.** Submit **one** pull request per bug fix or feature. A pull request should contain isolated changes pertaining to a single bug fix or feature implementation. **Do not** refactor or reformat code that is unrelated to your change. It is better to **submit many small pull requests** rather than a single large one. Enormous pull requests will take enormous amounts of time to review, or may be rejected altogether. 

- **Coordinate bigger changes.** For large and non-trivial changes, open an issue to discuss a strategy with the maintainers. Otherwise, you risk doing a lot of work for nothing!

- **Prioritize understanding over cleverness.** Write code clearly and concisely. Remember that source code usually gets written once and read often. Ensure the code is clear to the reader. The purpose and logic should be obvious to a reasonably skilled developer, otherwise you should add a comment that explains it.

- **Follow existing coding style and conventions.** Keep your code consistent with the style, formatting, and conventions in the rest of the code base. When possible, these will be enforced with a linter. Consistency makes it easier to review and modify in the future.

- **Include test coverage.** Add unit tests or UI tests when possible. Follow existing patterns for implementing tests.

- **Use the repo's default branch.** Branch from and [submit your pull request](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/proposing-changes-to-your-work-with-pull-requests/creating-a-pull-request) to the repo's default branch. Usually this is `main`, but it could be `dev`, `develop`, or `master`.

- **[Resolve any merge conflicts](https://help.github.com/en/github/collaborating-with-issues-and-pull-requests/resolving-a-merge-conflict-on-github)** that occur.

- **Keep PR in Draft** until ready for review. When PR ready for review assign PR to ourself and assing at least one reviewer

- **Promptly address any CI failures**. If your pull request fails to build or pass tests, please push another commit to fix it. 

- When writing comments, use properly constructed sentences, including punctuation.

- Use spaces, not tabs.

## :memo: Writing Commit Messages

Please [write a great commit message](https://chris.beams.io/posts/git-commit/).

1. Separate subject from body with a blank line
1. Limit the subject line to 50 characters
1. Capitalize the subject line
1. Add [Draft] prefix for PR not ready for review
1. Do not end the subject line with a period
1. Use the imperative mood in the subject line (example: "Fix networking issue")
1. Wrap the body at about 72 characters
1. Use the body to explain **why**, *not what and how* (the code shows that!)
1. If applicable, prefix the title with the relevant component name. (examples: "[Docs] Fix typo", "[Profile] Fix missing avatar")

```
[TAG] Short summary of changes in 50 chars or less

Add a more detailed explanation here, if necessary. Possibly give 
some background about the issue being fixed, etc. The body of the 
commit message can be several paragraphs. Further paragraphs come 
after blank lines and please do proper word-wrap.

Wrap it to about 72 characters or so. In some contexts, 
the first line is treated as the subject of the commit and the 
rest of the text as the body. The blank line separating the summary 
from the body is critical (unless you omit the body entirely); 
various tools like `log`, `shortlog` and `rebase` can get confused 
if you run the two together.

Explain the problem that this commit is solving. Focus on why you
are making this change as opposed to how or what. The code explains 
how or what. Reviewers and your future self can read the patch, 
but might not understand why a particular solution was implemented.
Are there side effects or other unintuitive consequences of this
change? Here's the place to explain them.

 - Bullet points are okay, too

 - A hyphen or asterisk should be used for the bullet, preceded
   by a single space, with blank lines in between

Note the fixed or relevant GitHub issues at the end:

Resolves: #123
See also: #456, #789
```

## :white_check_mark: Code Review

- **Review the code, not the author.** Look for and suggest improvements without disparaging or insulting the author. Provide actionable feedback and explain your reasoning.

- **You are not your code.** When your code is critiqued, questioned, or constructively criticized, remember that you are not your code. Do not take code review personally.

- **Always do your best.** No one writes bugs on purpose. Do your best, and learn from your mistakes.

- Kindly note any violations to the guidelines specified in this document. 

- **Use CREG (Code Review Emoji Guide)**  to give the reviewee added context and clarity to follow up on code review. 

### Emoji Legend

|     |   `:code:`   | Meaning                                                                                                                                                                             |
| :-: | :----------: | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 😃👍💯  |  `:smiley:` `:+1:` `:100:`  | I like this... <br /><br /> ...and I want the author to know it! This is a way to highlight positive parts of a code review.                                                        |
| 🔧  |  `:wrench:`  | I think this needs to be changed. <br /><br />This is a concern or suggested change/refactor that I feel is worth addressing.                                                       |
| ❓  | `:question:` | I have a question. <br /><br /> This should be a fully formed question with sufficient information and context that requires a response.                                            |
| 🤔💭 | `:thinking:` `:thought_balloon:` | Let me think out loud here for a minute. <br /><br /> I might express concern, suggest an alternative solution, or walk through the code in my own words to make sure I understand. |
| 🌱  | `:seedling:` | Planting a seed for future. <br /><br /> An observation or suggestion that is not a change request, but may have larger implications. Generally something to keep in mind for the future. |
| 📝  |   `:memo:`   | This is an explanatory note, fun fact, or relevant commentary that does not require any action.                                                                                     |
|  ⛏  |   `:pick:`   | This is a nitpick. <br /><br /> This does not require any changes and is often better left unsaid. This may include stylistic, formatting, or organization suggestions and should likely be prevented/enforced by linting if they really matter |
|  ♻️  | `:recycle:`  | Suggestion for refactoring. <br /><br /> Should include enough context to be actionable and not be considered a nitpick. |
|  🏕  | `:camping:`  | Here is an opportunity, not directly related to your changes, for us to leave the campground [code] cleaner than we found it.                                                       |
| 📌  | `:pushpin:`  | This is a concern that is _out of scope_ and should be staged appropriately for follow up.                                                                                          |


## :nail_care: Coding Style

Consistency is the most important. Following the existing Rust style, formatting, and naming conventions of the file you are modifying and of the overall project. Failure to do so will result in a prolonged review process that has to focus on updating the superficial aspects of your code, rather than improving its functionality and performance.

Style and format will be enforced with a linter when PR is crated.

## :pray: Credits

- https://github.com/jessesquires/.github/blob/main/CONTRIBUTING.md
- https://github.com/erikthedeveloper/code-review-emoji-guide