"""Read length-prefixed Itanium member names without guessing class identities."""

from __future__ import annotations

import re
from dataclasses import dataclass


@dataclass(frozen=True)
class MemberName:
	owner: str
	method: str
	parameters: str


def member_name(symbol: str) -> MemberName | None:
	match: re.Match[str] | None = re.match(r"_+ZN[KVRr]*", symbol)
	if match is None:
		return None
	offset: int = match.end()
	parts: list[str] = []
	while offset < len(symbol):
		length_match: re.Match[str] | None = re.match(r"[0-9]+", symbol[offset:])
		if length_match is None:
			break
		length: int = int(length_match[0])
		offset += length_match.end()
		if length == 0 or offset + length > len(symbol):
			return None
		parts.append(symbol[offset : offset + length])
		offset += length
	if len(parts) < 2 or offset >= len(symbol) or symbol[offset] != "E":
		# Template owners and constructors are outside this name decoder.
		return None
	return MemberName("::".join(parts[:-1]), parts[-1], symbol[offset + 1 :])


def registration_record(member: MemberName) -> str | None:
	if not member.method.startswith("Add"):
		return None
	match: re.Match[str] | None = re.fullmatch(
		r"PK?([0-9]+)([A-Za-z_][A-Za-z_0-9]*)", member.parameters
	)
	return match[2] if match is not None and len(match[2]) == int(match[1]) else None
