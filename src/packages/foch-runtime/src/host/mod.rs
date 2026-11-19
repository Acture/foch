//! Host DLL artifact validation and reusable inline-hooking / trampoline
//! mechanics (`image`, `prologue`, and on Windows x64 `link` and `query`).
//!
//! `DllMain`, the `VERSION.dll` forwarding exports, and the independent
//! `cdylib` remain isolated in `foch-runtime-host` so host exports never
//! pollute the CLI or native plugins.
//!
//! PE field layout: https://learn.microsoft.com/windows/win32/debug/pe-format

pub mod image;
#[cfg(all(windows, target_arch = "x86_64"))]
pub mod link;
pub mod prologue;
#[cfg(all(windows, target_arch = "x86_64"))]
pub mod query;

use std::{
	collections::BTreeMap,
	fs, io,
	path::{Path, PathBuf},
};

pub const DISTRIBUTED_DLL: &str = "foch_runtime.dll";
pub const BUILT_DLL: &str = "foch_runtime_host.dll";
pub const PROXY_EXPORTS: [&str; 17] = [
	"GetFileVersionInfoA",
	"GetFileVersionInfoByHandle",
	"GetFileVersionInfoExA",
	"GetFileVersionInfoExW",
	"GetFileVersionInfoSizeA",
	"GetFileVersionInfoSizeExA",
	"GetFileVersionInfoSizeExW",
	"GetFileVersionInfoSizeW",
	"GetFileVersionInfoW",
	"VerFindFileA",
	"VerFindFileW",
	"VerInstallFileA",
	"VerInstallFileW",
	"VerLanguageNameA",
	"VerLanguageNameW",
	"VerQueryValueA",
	"VerQueryValueW",
];

pub fn resolve(directory: &Path, explicit: Option<&Path>) -> io::Result<PathBuf> {
	if let Some(path) = explicit {
		return path.is_file().then(|| path.to_owned()).ok_or_else(|| {
			io::Error::new(
				io::ErrorKind::NotFound,
				format!("host DLL does not exist: {}", path.display()),
			)
		});
	}
	// A fresh development build must not be shadowed by an older distribution
	// DLL left beside the executable. Installed bundles carry only the latter.
	for name in [BUILT_DLL, DISTRIBUTED_DLL] {
		let path = directory.join(name);
		if path.is_file() {
			return Ok(path);
		}
	}
	Err(io::Error::new(
		io::ErrorKind::NotFound,
		format!(
			"cannot find {DISTRIBUTED_DLL} or {BUILT_DLL} beside {}; build foch-runtime-host or install its Windows host bundle",
			directory.display()
		),
	))
}

/// Freeze the exact validated bytes that will be staged as VERSION.dll.
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
	let bytes = fs::read(path).map_err(|error| {
		io::Error::new(
			error.kind(),
			format!(
				"cannot read host {}: {error}; build foch-runtime-host",
				path.display()
			),
		)
	})?;
	validate_host(&bytes).map_err(io::Error::other)?;
	Ok(bytes)
}

pub fn validate_host(bytes: &[u8]) -> Result<(), String> {
	let exports = exports(bytes)?;
	if exports.contains_key(foch_plugin_sdk::ENTRY) {
		return Err("host DLL also exports a plugin entry; rebuild the independent foch-runtime-host package".into());
	}
	for (index, name) in PROXY_EXPORTS.iter().enumerate() {
		if exports.get(*name) != Some(&((index + 1) as u32)) {
			return Err(format!(
				"host DLL is missing VERSION export {name} at ordinal {}; build foch-runtime-host",
				index + 1
			));
		}
	}
	Ok(())
}

pub fn validate_plugin(bytes: &[u8]) -> Result<(), String> {
	let exports = exports(bytes)?;
	if !exports.contains_key(foch_plugin_sdk::ENTRY) {
		return Err("plugin DLL is missing foch_plugin_get_api".into());
	}
	if PROXY_EXPORTS.iter().any(|name| exports.contains_key(*name)) {
		return Err("plugin DLL contains VERSION host exports; host and plugin packages must be independent".into());
	}
	Ok(())
}

struct Section {
	rva: usize,
	raw: usize,
	len: usize,
	executable: bool,
}

struct Pe<'a> {
	bytes: &'a [u8],
	headers: usize,
	sections: Vec<Section>,
}

impl Pe<'_> {
	fn map(&self, rva: usize, len: usize) -> Result<(usize, usize), String> {
		let mut matches = vec![];
		if rva < self.headers && rva.checked_add(len).is_some_and(|end| end <= self.headers) {
			matches.push((rva, self.headers));
		}
		for section in &self.sections {
			if let Some(offset) = rva.checked_sub(section.rva)
				&& offset
					.checked_add(len)
					.is_some_and(|end| end <= section.len)
			{
				matches.push((section.raw + offset, section.raw + section.len));
			}
		}
		match matches.as_slice() {
			[(offset, end)] if *end <= self.bytes.len() => Ok((*offset, *end)),
			_ => Err("PE RVA is absent, ambiguous or outside file-backed sections".into()),
		}
	}

	fn name(&self, rva: usize) -> Result<&str, String> {
		let (at, end) = self.map(rva, 1)?;
		let tail = &self.bytes[at..end.min(at + 256)];
		let size = tail
			.iter()
			.position(|byte| *byte == 0)
			.ok_or("unterminated PE export name")?;
		if size == 0 || !tail[..size].iter().all(u8::is_ascii) {
			return Err("invalid PE export name".into());
		}
		std::str::from_utf8(&tail[..size]).map_err(|error| error.to_string())
	}
}

fn slice(bytes: &[u8], at: usize, len: usize) -> Result<&[u8], String> {
	bytes
		.get(at..at.checked_add(len).ok_or("PE offset overflow")?)
		.ok_or_else(|| "truncated PE image".into())
}
fn word(bytes: &[u8], at: usize) -> Result<u16, String> {
	Ok(u16::from_le_bytes(slice(bytes, at, 2)?.try_into().unwrap()))
}
fn dword(bytes: &[u8], at: usize) -> Result<usize, String> {
	Ok(u32::from_le_bytes(slice(bytes, at, 4)?.try_into().unwrap()) as usize)
}

fn exports(bytes: &[u8]) -> Result<BTreeMap<String, u32>, String> {
	if slice(bytes, 0, 2)? != b"MZ" {
		return Err("DLL has no DOS header".into());
	}
	let pe = dword(bytes, 0x3c)?;
	slice(bytes, pe, 24)?;
	if slice(bytes, pe, 4)? != b"PE\0\0"
		|| word(bytes, pe + 4)? != 0x8664
		|| word(bytes, pe + 22)? & 0x2000 == 0
	{
		return Err("DLL must be an AMD64 PE image".into());
	}
	let optional = pe + 24;
	let optional_size = word(bytes, pe + 20)? as usize;
	let header = slice(bytes, optional, optional_size)?;
	if word(header, 0)? != 0x20b || header.len() < 120 || dword(header, 108)? == 0 {
		return Err("DLL has no PE32+ export directory".into());
	}
	let count = word(bytes, pe + 6)? as usize;
	if !(1..=96).contains(&count) {
		return Err("invalid PE section count".into());
	}
	let mut image = Pe {
		bytes,
		headers: dword(header, 60)?,
		sections: vec![],
	};
	slice(bytes, 0, image.headers)?;
	let table = optional + optional_size;
	slice(bytes, table, count * 40)?;
	for index in 0..count {
		let at = table + index * 40;
		let section = Section {
			rva: dword(bytes, at + 12)?,
			raw: dword(bytes, at + 20)?,
			len: dword(bytes, at + 16)?,
			executable: dword(bytes, at + 36)? & 0x2000_0000 != 0,
		};
		slice(bytes, section.raw, section.len)?;
		image.sections.push(section);
	}
	let export_rva = dword(header, 112)?;
	let export_size = dword(header, 116)?;
	if export_rva == 0 || export_size < 40 {
		return Err("DLL has no export table".into());
	}
	let export_end = export_rva
		.checked_add(export_size)
		.ok_or("PE export directory overflow")?;
	let export = image.map(export_rva, export_size)?.0;
	let base = dword(bytes, export + 16)?;
	let functions = dword(bytes, export + 20)?;
	let names = dword(bytes, export + 24)?;
	if functions == 0 || functions > 65536 || names == 0 || names > functions {
		return Err("invalid PE export counts".into());
	}
	let addresses = image.map(dword(bytes, export + 28)?, functions * 4)?.0;
	let name_pointers = image.map(dword(bytes, export + 32)?, names * 4)?.0;
	let ordinals = image.map(dword(bytes, export + 36)?, names * 2)?.0;
	let mut result = BTreeMap::new();
	for index in 0..names {
		let name = image.name(dword(bytes, name_pointers + index * 4)?)?;
		let ordinal = word(bytes, ordinals + index * 2)? as usize;
		if ordinal >= functions {
			return Err("PE export ordinal is out of bounds".into());
		}
		let target = dword(bytes, addresses + ordinal * 4)?;
		image.map(target, 1)?;
		if (export_rva..export_end).contains(&target)
			|| !image.sections.iter().any(|section| {
				section.executable && target >= section.rva && target - section.rva < section.len
			}) {
			return Err("DLL export is forwarded or outside executable bytes".into());
		}
		let ordinal = u32::try_from(base.checked_add(ordinal).ok_or("PE ordinal overflow")?)
			.map_err(|error| error.to_string())?;
		if result.insert(name.to_owned(), ordinal).is_some() {
			return Err("duplicate PE export name".into());
		}
	}
	Ok(result)
}
