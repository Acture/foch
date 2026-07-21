# Mod Merge Semantics

Ubiquitous language for describing foch merge behavior and the evidence used to evaluate it.

## Language

**Common Applicability Hypothesis**:
The falsifiable claim that a structured merge kernel proven on event files can execute every `common/**` corpus unit when guided by that unit's content-family semantics, without a separate merge algorithm for each family. It does not claim human-equivalent output quality.
_Avoid_: Common support, common is solved

**Directory Module Hypothesis**:
The provisional assumption that files under the same `common/<folder>` form one semantic merge unit. It is an initial probe boundary to validate against game loading behavior, not an established loader fact.
_Avoid_: Folder is a module, verified load policy

**Structured Applicability**:
The invariant that every parseable Clausewitz-script unit can be represented and processed by the shared structured merge model. Content families specialize merge semantics; they are not a support allowlist.
_Avoid_: Supported family list, events-only support

**Manual Resolution Required**:
A terminal result for edits that the structured model understands but no declared semantic policy can resolve safely. It is handled evidence, but not an accepted automatic merge.
_Avoid_: Unsupported, merge crash, automatic fallback

**Merge Unit Publication**:
The release of a merge unit into the generated mod after every semantic conflict in that unit has been resolved. Conflict evidence and candidate previews are resolution artifacts, never published output.
_Avoid_: Tentative output, best-effort publication

**Merge Unit**:
The smallest semantic aggregate that foch merges, reviews, resolves, and publishes atomically. A unit may be one effective file or a multi-file definition module, depending on game loading semantics.
_Avoid_: Output file, source file

**Accepted Automatic Merge**:
A conflict-free result supported by family-aware evidence as semantically equivalent to or better than the human compatch. Exact textual or structural identity with the human output is not required.
_Avoid_: Exact human copy, parses successfully

**Common Applicability Gate**:
The first corpus gate requiring every `common/**` unit to reach a classified structured outcome without being unsupported, crashing, timing out, or producing malformed structure. It establishes coverage and failure evidence without prescribing an automatic-acceptance rate.
_Avoid_: Common quality gate, common rollout

**Quality Matrix**:
The per-unit evidence separating accepted automatic merges, manual-resolution requirements, and semantic mismatches across a fixed corpus denominator.
_Avoid_: Pass rate, aggregate score

**Review-Required Content Family**:
A content family with a known semantic mismatch whose units still execute through Structured but require human approval before publication. It is neither unsupported nor silently routed to Legacy.
_Avoid_: Disabled family, Legacy fallback
