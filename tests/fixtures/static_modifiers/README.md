# Static-modifier product fixtures

Synthetic EU4 ancestor `base/` and immutable source mods for P-553/P-556.
The tests build and install a snapshot from `base/`, then create ordered project
manifests in scratch space pointing at these source directories. The ancestor is
never inferred from a missing game file or introduced as a fake first mod.

`shared` starts with tax and morale at `0.10`, discipline at `0.05`.
`base_only` is unchanged in every source. `tax` also supplies localisation so
unsafe module deferral must still commit an independent safe file.

- `tax` / `morale`: independent field changes.
- `vanilla`: unchanged carrier, including when placed last.
- `equal`: the same tax change as `tax`, with its own provenance.
- `up` / `down`: incompatible final tax values, including opposite changes.
- `adapter`: explicitly depends on `tax` and `up` and supplies a final value.
- `tax_adapter`: depends only on `tax`; independent morale must survive.
- `last`: has the adapter's value without its dependency declaration.
- `deleted`: removes the tax field while retaining the definition.

The shared matrix and semantic assertions live in
`tests/support/static_modifiers.rs`. Both `tests/merge_static_modifiers.rs` and
the CLI's `cli_integration` target consume it. Output bytes, AST values,
definition references, source attribution, conflict IDs and input immutability
are checked; exit status alone is not acceptance.

These cases establish Foch's behavior under the existing family descriptor and
dependency DAG. They do not independently verify EU4 runtime duplicate-definition
or filename precedence rules, or prove that arbitrary numeric changes are
compatible. See `docs/static-modifiers-verification.md` for the observed failure,
fix, real-input boundary and validation commands.
