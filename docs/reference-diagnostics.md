# Reference diagnostics

`foch check` explains why a resource reference does not resolve. A name that
nothing declares is not automatically a mistake: the file that would declare
it may have failed to parse, the mod that declares it may be missing from the
playset, or a later mod may have replaced the directory it lives in. Each
cause wants a different fix, so the analysis tells them apart instead of
reporting every unresolved name as "not found".

The classification needs the playset, not the file alone. A name is only
missing when every file that could declare it was read. A file that failed to
parse, a base game that is not part of the input, and a family the rules do
not describe all leave the answer unknown, and an unknown answer is never
reported as a missing name.

## Rules

| Rule id | Severity | Channel | Meaning |
| --- | --- | --- | --- |
| `unresolved-reference-not-declared` | error | strict | No file of the effective playset declares the name. |
| `unresolved-reference-replaced` | error | strict | The name is declared, but the playset keeps none of the declarations: a later mod replaces the directory they live in. |
| `unresolved-reference-missing-texture` | error | strict | A sprite is declared, but the texture file it names is not in the playset. |
| `unresolved-reference-broken-source` | warning | advisory | A file that could declare the name did not parse, so whether it does is unknown. |
| `unresolved-reference-missing-dependency` | warning | advisory | The referring mod depends on a mod the playset does not load; the name may well be declared there. |
| `unresolved-reference-unloadable-sprite` | warning | advisory | The sprite is declared, but names no texture: the engine has nothing to load. |
| `unresolved-reference-unknown` | info | advisory | The analysis cannot tell: the scope does not cover every file that could declare the name, or the rules name a kind of definition foch has no rule for. |

Only the failures the analysis is sure of are strict. A broken source, a
missing dependency and an unusable sprite are leads worth reading rather than
verdicts, so they stay advisory and leave the check's exit code alone.

The generic `unresolved-call-target` finding reports an event reference that
does not resolve. When the reference analysis explains the same site, it
replaces that finding: the explanation says whether the reference is broken
and why, which the generic report cannot.

## What a diagnostic carries

Every reference finding names:

- the **mod** that writes the reference, in `mod_id`;
- the **file**, relative to that mod's root, in `path`;
- the **position** in the file, in `line` and `column`;
- the **related providers** — every declaration of the name the analysis
  found, effective or not, so an override chain is visible;
- the **base and input identity** the analysis read them from, so a finding
  can be tied to the analysis that produced it.

The last three travel in the finding's `evidence`, in the same
`key=value; key=value` shape the other rules use:

```
reference=building_definition name=my_building reason=not_declared providers=[] base=eu4@1.37.5.0 input=dlc_load.json
```

`base` names the base game installation the base data snapshot was built
from, and is `absent` when the base game is not part of the input. `input`
names the playset the analysis read.

## What is diagnosed

| Reference | How it is resolved |
| --- | --- |
| Event id | The event's id, with the namespace prefix checked against the namespaces the event files declare. A dot in an id is not by itself evidence that the namespace exists, and a namespace declared in another file counts. |
| Definition of a schema type | The typed effective definitions of the family the schema's `<type>` marker names, plus the base game. A value the engine substitutes at run time is not a reference. |
| GUI sprite | The sprite declarations of the interface files, then the texture file the sprite names, against the playset's file inventory. |

The event namespace is checked first: when the namespace is not declared, that
is the root cause, and the event itself is not reported as missing.

## What is not claimed

- A name nothing declares is not called missing while a file of its family
  failed to parse, while the base game is not part of the input, or while the
  rules do not describe the kind of reference.
- A sprite's texture is not called missing while the file inventory does not
  cover the whole playset: a narrowed inventory proves nothing about absence.
- A dynamic value — a parameter, an event target, a run-time substitution — is
  not a reference at all.
- A legal override passes: when the playset keeps one of several
  declarations, the reference resolves.

A kind of definition the rules name but foch records no definitions of is
reported once per mod and type, not once per reference: the lead is that
foch cannot judge that kind of reference, and repeating it buries the rest.

## Implementation

`src/packages/foch/src/game/eu4/analysis/reference.rs` holds the
classification. It reuses the schema (for the type a value names), the
semantic index (for the declarations and the references) and the resolved
input (for the playset's load order, `replace_path` prefixes, dependencies and
file inventory).
