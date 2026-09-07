from __future__ import annotations

from dataclasses import dataclass
from typing import Literal


@dataclass(frozen=True)
class Function:
	address: int
	end: int
	names: tuple[str, ...]


@dataclass(frozen=True)
class Reference:
	address: int
	names: tuple[str, ...] = ()
	string: str | None = None


@dataclass(frozen=True)
class Instruction:
	address: int
	mnemonic: str
	operands: tuple[str, ...]
	call: Reference | None = None
	data: tuple[Reference, ...] = ()


@dataclass(frozen=True)
class FunctionEvidence:
	function: Function
	instructions: tuple[Instruction, ...]
	pseudocode: str | None


@dataclass(frozen=True)
class Evidence:
	function: str
	address: int
	observation: str


@dataclass(frozen=True)
class ResourcePath:
	base: str
	suffix: str


@dataclass(frozen=True)
class ResourceScope:
	registry: str
	paths: tuple[ResourcePath, ...]
	file_filter: str | None


@dataclass(frozen=True)
class DefinitionIdentity:
	namespace: str
	key_member_offset: int
	key_type: str
	source_key: str | None


@dataclass(frozen=True)
class FamilyRules:
	binary_sha256: str
	game_version: str
	registry: str
	resource_scope: ResourceScope | None
	definition_identity: DefinitionIdentity | None
	status: Literal["candidate", "unknown"]
	evidence: tuple[Evidence, ...]
	unknowns: tuple[str, ...]
	format_version: int = 1
