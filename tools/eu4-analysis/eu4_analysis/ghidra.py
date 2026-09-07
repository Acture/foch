"""PyGhidra adapter; Java dependencies are loaded only when opening a session."""

from __future__ import annotations

import logging
import re
import time
from contextlib import ExitStack
from pathlib import Path
from types import TracebackType
from typing import TYPE_CHECKING

from .macho import BinaryImage, read_binary
from .models import Function, FunctionEvidence, Instruction, Reference

if TYPE_CHECKING:
	from ghidra.program.model.listing import Program

LOGGER: logging.Logger = logging.getLogger(__name__)


def validate_output_directory(binary: Path, installation: Path, output: Path) -> None:
	binary = binary.resolve()
	game_directory: Path = next(
		(parent.parent for parent in binary.parents if parent.suffix == ".app"),
		binary.parent,
	)
	if output.resolve().is_relative_to(game_directory):
		raise ValueError("output must be outside the game installation")
	if output.resolve().is_relative_to(installation.resolve()):
		raise ValueError("output must be outside the Ghidra installation")


class GhidraSession:
	"""One JVM/project session, shared by all stages of a bounded investigation."""

	def __init__(
		self, binary: Path, installation: Path, workspace: Path, timeout: int = 45
	) -> None:
		self.image: BinaryImage = read_binary(binary)
		self.installation: Path = installation.resolve()
		self.workspace: Path = workspace.resolve()
		self.timeout: int = timeout
		self.program: Program
		self._resources: ExitStack = ExitStack()
		self._cache: dict[int, FunctionEvidence] = {}
		self._function_starts: frozenset[int] = frozenset(self.image.function_starts)
		self._dirty: bool = False
		if timeout <= 0:
			raise ValueError("timeout must be positive")
		validate_output_directory(self.image.path, self.installation, self.workspace)

	def __enter__(self) -> GhidraSession:
		import pyghidra
		from pyghidra import HeadlessPyGhidraLauncher

		started: float = time.perf_counter()
		for directory in ("settings", "cache", "temp", self.image.sha256):
			(self.workspace / directory).mkdir(parents=True, exist_ok=True)
		launcher: HeadlessPyGhidraLauncher = HeadlessPyGhidraLauncher(
			install_dir=self.installation
		)
		if launcher.app_info.version != "12.1.3":
			raise ValueError("this backend requires Ghidra 12.1.3")
		if not pyghidra.started():
			launcher.add_vmargs(
				"-Xmx2G",
				f"-Dapplication.settingsdir={self.workspace / 'settings'}",
				f"-Dapplication.cachedir={self.workspace / 'cache'}",
				f"-Dapplication.tempdir={self.workspace / 'temp'}",
			)
			launcher.start()
		LOGGER.info("PyGhidra ready in %.2fs", time.perf_counter() - started)
		# ExitStack also releases resources if opening/importing the program fails.
		with ExitStack() as resources:
			project = resources.enter_context(
				pyghidra.open_project(
					self.workspace / self.image.sha256, "eu4", create=True
				)
			)
			if project.getProjectData().getFile("/eu4") is None:
				LOGGER.info(
					"Importing executable; whole-program auto-analysis disabled"
				)
				loader = (
					pyghidra.program_loader()
					.project(project)
					.source(str(self.image.path))
					.name("eu4")
				)
				with loader.monitor(
					pyghidra.task_monitor(self.timeout)
				).load() as loaded:
					loaded.save(pyghidra.task_monitor(self.timeout))
			self.program = resources.enter_context(
				pyghidra.program_context(project, "/eu4")
			)
			if str(self.program.getExecutableSHA256()).lower() != self.image.sha256:
				raise ValueError("imported binary hash differs from input")
			self._resources = resources.pop_all()
		return self

	def __exit__(
		self,
		exc_type: type[BaseException] | None,
		exc: BaseException | None,
		traceback: TracebackType | None,
	) -> None:
		import pyghidra

		try:
			if self._dirty:
				self.program.save(
					"Bounded function evidence", pyghidra.task_monitor(self.timeout)
				)
		finally:
			self._resources.close()

	def find(self, pattern: str) -> tuple[Function, ...]:
		addresses: set[int] = {
			address
			for symbol in self.program.getSymbolTable().getSymbolIterator(pattern, True)
			if (address := int(symbol.getAddress().getOffset()))
			in self._function_starts
		}
		return tuple(self.at(address) for address in sorted(addresses))

	def at(self, address: int) -> Function:
		end: int = self.image.function_end(address)
		location = (
			self.program.getAddressFactory()
			.getDefaultAddressSpace()
			.getAddress(address)
		)
		names: tuple[str, ...] = tuple(
			str(symbol.getName())
			for symbol in self.program.getSymbolTable().getSymbols(location)
		)
		return Function(address, end, names)

	def _reference(self, address: int) -> Reference:
		location = (
			self.program.getAddressFactory()
			.getDefaultAddressSpace()
			.getAddress(address)
		)
		names: tuple[str, ...] = tuple(
			str(symbol.getName())
			for symbol in self.program.getSymbolTable().getSymbols(location)
		)
		block = self.program.getMemory().getBlock(location)
		literal: str | None = None
		if block is not None and "cstring" in str(block.getName()).lower():
			length: int = min(512, int(block.getEnd().getOffset()) - address + 1)
			buffer: bytearray = bytearray()
			for offset in range(length):
				byte: int = (
					int(self.program.getMemory().getByte(location.add(offset))) & 255
				)
				if byte == 0:
					if all(32 <= value < 127 for value in buffer):
						literal = buffer.decode("ascii")
					break
				buffer.append(byte)
		return Reference(address, names, literal)

	def inspect(
		self, function: Function, *, pseudocode: bool = True
	) -> FunctionEvidence:
		if function.end != self.image.function_end(function.address):
			raise ValueError("function range differs from binary metadata")
		import pyghidra
		from ghidra.app.cmd.disassemble import DisassembleCommand
		from ghidra.app.decompiler import DecompInterface
		from ghidra.program.model.address import AddressSet
		from ghidra.program.model.symbol import SourceType

		cached: FunctionEvidence | None = self._cache.get(function.address)
		if cached is not None and (not pseudocode or cached.pseudocode is not None):
			return cached
		started: float = time.perf_counter()
		LOGGER.info(
			"Inspecting %#x (%s)",
			function.address,
			function.names[0] if function.names else "unnamed",
		)
		space = self.program.getAddressFactory().getDefaultAddressSpace()
		address = space.getAddress(function.address)
		end = space.getAddress(function.end - 1)
		block = self.program.getMemory().getBlock(address)
		if block is None or not block.isExecute() or not block.contains(end):
			raise ValueError(
				"function range must lie within one executable memory block"
			)
		body = AddressSet(address, end)
		with pyghidra.transaction(self.program, "Inspect bounded function"):
			command = DisassembleCommand(address, body, True)
			command.enableCodeAnalysis(False)
			if not command.applyTo(self.program, pyghidra.task_monitor(self.timeout)):
				raise RuntimeError(str(command.getStatusMsg()))
			loaded_function = self.program.getFunctionManager().getFunctionAt(address)
			if loaded_function is None:
				loaded_function = self.program.getFunctionManager().createFunction(
					function.names[0]
					if function.names
					else f"sub_{function.address:x}",
					address,
					body,
					SourceType.IMPORTED,
				)
			elif not loaded_function.getBody().equals(body):
				raise ValueError(
					"saved function range differs from binary metadata; use a fresh workspace"
				)
		instructions: list[Instruction] = []
		for instruction in self.program.getListing().getInstructions(body, True):
			call: Reference | None = None
			data: list[Reference] = []
			for reference in instruction.getReferencesFrom():
				if not reference.getToAddress().isMemoryAddress():
					continue
				if reference.getReferenceType().isCall():
					call = self._reference(int(reference.getToAddress().getOffset()))
				elif reference.getReferenceType().isData():
					data.append(
						self._reference(int(reference.getToAddress().getOffset()))
					)
			# LEA operands need not have stored data references without auto-analysis.
			for index in range(instruction.getNumOperands()):
				match: re.Match[str] | None = re.search(
					r"\[(0x[0-9a-fA-F]+)\]",
					str(instruction.getDefaultOperandRepresentation(index)),
				)
				if match is not None:
					value: int = int(match[1], 16)
					if all(reference.address != value for reference in data):
						data.append(self._reference(value))
			instructions.append(
				Instruction(
					int(instruction.getAddress().getOffset()),
					str(instruction.getMnemonicString()),
					tuple(
						str(instruction.getDefaultOperandRepresentation(index))
						for index in range(instruction.getNumOperands())
					),
					call,
					tuple(data),
				)
			)
		c_text: str | None = None
		if pseudocode:
			decompiler = DecompInterface()
			try:
				if not decompiler.openProgram(self.program):
					raise RuntimeError(str(decompiler.getLastMessage()))
				result = decompiler.decompileFunction(
					loaded_function, self.timeout, pyghidra.task_monitor(self.timeout)
				)
				if (
					not result.decompileCompleted()
					or result.getDecompiledFunction() is None
				):
					raise RuntimeError(str(result.getErrorMessage()))
				c_text = str(result.getDecompiledFunction().getC())
			finally:
				decompiler.dispose()
		self._dirty = True
		evidence: FunctionEvidence = FunctionEvidence(
			function, tuple(instructions), c_text
		)
		self._cache[function.address] = evidence
		LOGGER.info(
			"Inspected %#x in %.2fs", function.address, time.perf_counter() - started
		)
		return evidence
