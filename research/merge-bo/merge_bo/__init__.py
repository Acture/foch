"""Research harness: Bayesian optimization of EU4 mod-merge validity.

Drives the `foch` CLI as a black box (priority ordering in -> conflict graph +
materialized merge out) and, later, an EU4 observer-mode error oracle. See
`docs`/the design plan for the two-track structure (method core + prevalence gate
ladder) and the gate-driven decision nodes.
"""
