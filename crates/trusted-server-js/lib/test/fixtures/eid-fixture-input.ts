/**
 * The fixed input behind `auction-body-eids.txt` and `ts-eids-cookie.txt`
 * (spec 7.1 rule 8). The middle entry's UID `id` holds a lone UTF-16
 * surrogate, which `JSON.stringify` writes as the escape `\ud800` and the
 * server drops for that entry only. The first entry has non-Latin-1 text in
 * `inserter` and in a UID `ext`. The last has an EID-level `ext`, which the
 * cookie omits. Key order matters: the fixtures are exact bytes.
 */
export const FIXTURE_EIDS = [
  {
    source: 'first.example.com',
    inserter: 'inserter-€.example.com',
    matcher: 'first.example.com',
    mm: 3,
    uids: [{ id: 'first-uid', atype: 1, ext: { label: '日本' } }],
  },
  { source: 'bad.example.com', uids: [{ id: 'bad-\ud800-uid', atype: 1 }] },
  {
    source: 'last.example.com',
    ext: { segments: ['s1'] },
    uids: [{ id: 'last-uid', atype: 3 }],
  },
];
