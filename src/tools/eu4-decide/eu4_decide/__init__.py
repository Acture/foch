"""Offline GRPO trainer for the EU4 decide policy.

The environment and the world live in the Rust crate; this package drives
`env_server` over stdio and trains the shared candidate scorer with a
clipped policy gradient.
"""

from ._grpo import main

__all__ = ["main"]
