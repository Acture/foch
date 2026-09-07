"""Conservative x86_64 argument tracing for EU4 C++ registry candidates.

Branches discard temporary values. Directory origins and single-assignment
saved inputs survive as candidates, without proving reachability.
"""

from __future__ import annotations

import re
from collections import Counter
from dataclasses import dataclass

from .models import FunctionEvidence, Instruction, ResourcePath
from .symbols import member_name


@dataclass(frozen=True)
class Pointer:
	base: str
	offset: int = 0


@dataclass(frozen=True)
class Registry:
	name: str


@dataclass(frozen=True)
class StringValue:
	source: Pointer | str | ResourcePath


Value = Pointer | Registry | StringValue | str | int | None


@dataclass(frozen=True)
class Call:
	address: int
	names: tuple[str, ...]
	arguments: tuple[Value, ...]


@dataclass(frozen=True)
class Trace:
	calls: tuple[Call, ...]


ARGUMENTS: tuple[str, ...] = ("RDI", "RSI", "RDX", "RCX", "R8", "R9")
SAVED: frozenset[str] = frozenset(("RBX", "R12", "R13", "R14", "R15"))
REGISTER: re.Pattern[str] = re.compile(r"R(?:AX|BX|CX|DX|SI|DI|BP|SP|[89]|1[0-5])\Z")


def register(text: str) -> str | None:
	name: str = text.upper()
	if name.startswith("E"):
		name = "R" + name[1:]
	elif re.fullmatch(r"R(?:[89]|1[0-5])D", name):
		name = name[:-1]
	return name if REGISTER.fullmatch(name) else None


def has_name(names: tuple[str, ...], *parts: str) -> bool:
	return any(all(part in name for part in parts) for name in names)


def memory_expression(text: str) -> str:
	match: re.Match[str] | None = re.search(r"\[([^]]+)\]", text)
	return match[1].replace(" ", "").replace("+-", "-") if match else ""


def trace(
	function: FunctionEvidence,
	registry: str | None = None,
	*,
	arguments: tuple[Value, ...] | None = None,
) -> Trace:
	registers: dict[str, Value] = {
		name: Pointer(f"arg{index}")
		if arguments is None
		else (arguments[index] if index < len(arguments) else None)
		for index, name in enumerate(ARGUMENTS)
	}
	memory: dict[Pointer, Value] = {}
	origins: dict[Pointer, Value] = {}
	invariants: dict[str, Value] = {}
	calls: list[Call] = []
	write_counts: Counter[str] = Counter(
		name
		for instruction in function.instructions
		if instruction.mnemonic not in {"CMP", "TEST", "PUSH", "POP", "CALL"}
		and instruction.operands
		if (name := register(instruction.operands[0])) is not None
	)
	# Only immutable spills of incoming pointers may survive a branch.
	stack_writes: Counter[str] = Counter(
		memory_expression(instruction.operands[0])
		for instruction in function.instructions
		if instruction.operands
		and "[RBP" in instruction.operands[0]
		and instruction.mnemonic not in {"CMP", "TEST", "PUSH", "CALL"}
	)

	def assign(name: str, value: Value) -> None:
		registers[name] = value
		if (
			name in SAVED
			and write_counts[name] == 1
			and isinstance(value, Pointer)
			and value.base.startswith("arg")
		):
			invariants[name] = value

	def pointer(text: str, instruction: Instruction) -> Pointer | str | int | None:
		expression: str = memory_expression(text)
		if re.fullmatch(r"0x[0-9a-fA-F]+", expression):
			address: int = int(expression, 16)
			for reference in instruction.data:
				if reference.address == address and reference.string is not None:
					return reference.string
			return Pointer(f"address:{address:x}")
		base: Pointer | None = None
		offset: int = 0
		for term in expression.replace("-", "+-").split("+"):
			if re.fullmatch(r"-?(?:0x[0-9a-fA-F]+|[0-9]+)", term):
				offset += int(term, 0)
				continue
			parts: re.Match[str] | None = re.fullmatch(
				r"([A-Z0-9]+)(?:\*(0x[0-9a-fA-F]+|[0-9]+))?", term
			)
			if parts is None:
				return None
			value: Value = (
				Pointer("frame") if parts[1] == "RBP" else registers.get(parts[1])
			)
			scale: int = int(parts[2], 0) if parts[2] else 1
			if isinstance(value, int):
				offset += value * scale
			elif isinstance(value, Pointer) and scale == 1 and base is None:
				base = value
			else:
				return None
		return Pointer(base.base, base.offset + offset) if base is not None else offset

	def value(text: str, instruction: Instruction) -> Value:
		name: str | None = register(text)
		if name is not None:
			return registers.get(name)
		if "[" in text:
			location = pointer(text, instruction)
			if isinstance(location, Pointer):
				return memory.get(
					location, Pointer(f"*({location.base}+{location.offset:#x})")
				)
			# A load still produces a value when its indexed address is unknown.
			# Only subsequent constant member offsets are usable as candidates.
			return Pointer(f"*unknown:{instruction.address:x}")
		if re.fullmatch(r"-?(?:0x[0-9a-fA-F]+|[0-9]+)", text):
			return int(text, 0)
		return None

	for instruction in function.instructions:
		op: str = instruction.mnemonic
		operands: tuple[str, ...] = instruction.operands
		if op == "CALL":
			names: tuple[str, ...] = instruction.call.names if instruction.call else ()
			raw: tuple[Value, ...] = tuple(registers.get(name) for name in ARGUMENTS)
			resolved: tuple[Value, ...] = tuple(
				memory.get(arg, arg) if isinstance(arg, Pointer) else arg for arg in raw
			)
			calls.append(Call(instruction.address, names, resolved))
			result: Value = Pointer(f"result:{instruction.address:x}")
			destination: Value = raw[0]
			if isinstance(destination, Pointer):
				origins.pop(destination, None)
			accessors: set[str] = {
				member.owner
				for name in names
				if (member := member_name(name)) is not None
				and member.method == "AccessInstance"
			}
			if registry is None and len(accessors) == 1:
				result = Registry(next(iter(accessors)))
			elif registry is not None and has_name(names, registry, "AccessInstance"):
				result = Registry(registry)
			elif (
				(
					has_name(names, "CDirectorySettings", "GetOriginalDirectory")
					or has_name(names, "CDirectorySettings", "GetModDirectory")
				)
				and isinstance(destination, Pointer)
				and isinstance(raw[2], int)
			):
				method: str = (
					"GetOriginalDirectory"
					if has_name(names, "GetOriginalDirectory")
					else "GetModDirectory"
				)
				memory[destination] = StringValue(
					ResourcePath(f"CDirectorySettings::{method}({raw[2]})", "")
				)
				origins[destination] = memory[destination]
			elif any(
				re.search(r"7CStringC[12]ERKS_", name) for name in names
			) and isinstance(destination, Pointer):
				memory[destination] = resolved[1]
			elif any(
				re.search(r"7CStringC[12]EPKc", name) for name in names
			) and isinstance(destination, Pointer):
				memory[destination] = (
					StringValue(raw[1]) if isinstance(raw[1], (str, Pointer)) else None
				)
			elif has_name(names, "7CStringpLEPKc") and isinstance(destination, Pointer):
				current: Value = memory.get(destination)
				if (
					isinstance(current, StringValue)
					and isinstance(current.source, ResourcePath)
					and isinstance(raw[1], str)
				):
					memory[destination] = StringValue(
						ResourcePath(
							current.source.base, current.source.suffix + raw[1]
						)
					)
				else:
					memory[destination] = None
			else:
				for argument in raw:
					if isinstance(argument, Pointer):
						memory.pop(argument, None)
						origins.pop(argument, None)
			for name in ("RAX", "RCX", "RDX", "RSI", "RDI", "R8", "R9", "R10", "R11"):
				registers.pop(name, None)
			registers["RAX"] = result
		elif op.startswith("J") or op in {"RET", "RETF"}:
			registers = dict(invariants)
			memory = dict(origins)
		elif len(operands) >= 2 and op in {"MOV", "LEA", "XOR", "ADD", "SUB"}:
			destination_name: str | None = register(operands[0])
			new_value: Value = (
				pointer(operands[1], instruction)
				if op == "LEA"
				else value(operands[1], instruction)
			)
			if op == "XOR":
				new_value = 0 if operands[0] == operands[1] else None
			elif op in {"ADD", "SUB"}:
				previous: Value = (
					registers.get(destination_name) if destination_name else None
				)
				if isinstance(previous, Pointer) and isinstance(new_value, int):
					new_value = Pointer(
						previous.base,
						previous.offset + (new_value if op == "ADD" else -new_value),
					)
				else:
					new_value = None
			if destination_name is not None:
				assign(destination_name, new_value)
			else:
				destination_location = pointer(operands[0], instruction)
				if isinstance(destination_location, Pointer):
					memory[destination_location] = new_value if op == "MOV" else None
					origins.pop(destination_location, None)
					if (
						op == "MOV"
						and destination_location.base == "frame"
						and stack_writes[memory_expression(operands[0])] == 1
						and isinstance(new_value, Pointer)
						and new_value.base.startswith("arg")
					):
						origins[destination_location] = new_value
		elif operands and op not in {"CMP", "TEST", "PUSH", "NOP"}:
			name = register(operands[0])
			if name is not None:
				registers.pop(name, None)
			else:
				location = pointer(operands[0], instruction)
				if isinstance(location, Pointer):
					memory.pop(location, None)
					origins.pop(location, None)
	return Trace(tuple(calls))
