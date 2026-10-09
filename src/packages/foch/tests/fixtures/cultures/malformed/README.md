# Cultural Influence malformed-input examples

These deliberately invalid, minimized inputs preserve error shapes observed in
Workshop item `1915164313` (Cultural Influence Vanilla). Keep the malformed
inputs intact; regression tests apply exact edits to separate in-memory copies.

The parser and culture repair unit tests consume these files. Public merge and
CLI tests additionally verify source-bound repairs through analysis and commit.

- `missing_group_openers.txt` retains the four bare culture-group names seen in
  the source. Tests insert the missing `= {` and revalidate the culture hierarchy.
- `extra_closing_brace.txt` contains an unmatched closing brace. The generic
  parser reports its location and continues parsing the following groups.
- `premature_group_close.txt` is balanced but puts a name list outside its
  culture group. It captures why balanced braces alone do not establish a
  correct repair. This third example is synthetic, motivated by the remaining
  top-level name lists in the seven-edit comparison input.

The examples omit the original mod's large name lists and gameplay content.
They are not replacement files or a compatibility patch.

See [the investigation](../../../../docs/research/2026-10-01-culture-input-tolerance.md)
for source identity, observed results, and the limits of the comparison.
