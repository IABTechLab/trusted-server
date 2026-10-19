# Documentation proposal evidence

The hunks at the end of this file are proposed edits to the Trusted Server
documentation for the merged commit named there. For each hunk, find the merged
source that justifies it.

Write `<work-dir>/evidence.json` with exactly this shape, and change no
other file:

```json
{
  "hunks": [
    { "id": "<hunk id>", "source": "<path>:<line>", "reason": "<one sentence>" }
  ]
}
```

- Copy `id` exactly from the hunk list.
- `source` is a repository-relative path and line at the merged commit that
  shows the documented behavior.
- `reason` is one sentence a reviewer can check against that source.
- If no source justifies a hunk, omit it. The reviewer will be told the hunk
  has no cited source.
