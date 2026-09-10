# Contiguous Generated Slot Tables Design

## Problem

`splice_creative_slots` parses rendered slots in a temporary `toml_edit::DocumentMut` and moves its `ArrayOfTables` into the target document. Parsed tables retain document-local numeric positions. Those positions collide with positions in the target document, so serialization can interleave generated slot and provider tables with unrelated top-level tables even though the resulting TOML remains semantically valid.

## Design

Before insertion, assign the generated slot tables and all nested provider tables the target `[creative_opportunities]` table's document position. `toml_edit` performs a stable position sort, so equal positions retain traversal order: the creative table, each slot, and that slot's provider tables remain contiguous. For a newly created creative section, allocate an anchor after the greatest existing parsed-table position.

The update continues to preserve unrelated values, comments, line endings, and semantic table ownership. It does not reformat existing operator-authored content or modify slot inference.

## Testing

Add a regression fixture with a late `[creative_opportunities]` section and unrelated tables whose positions overlap those from the temporary generated document. Assert that the parent, generated slots, and provider subtables serialize contiguously before the next unrelated table. Retain the existing semantic-preservation and CRLF tests, then run the CLI test suite, formatting, and native CLI clippy.
