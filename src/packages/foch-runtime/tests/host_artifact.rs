use foch_runtime::host::{self, PROXY_EXPORTS};
use std::path::Path;

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
	bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn dll(names: &[&str]) -> Vec<u8> {
	let mut bytes = vec![0; 0x1200];
	bytes[..2].copy_from_slice(b"MZ");
	put32(&mut bytes, 0x3c, 0x80);
	bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
	put16(&mut bytes, 0x84, 0x8664);
	put16(&mut bytes, 0x86, 1);
	put16(&mut bytes, 0x94, 240);
	put16(&mut bytes, 0x96, 0x2000);
	let optional = 0x98;
	put16(&mut bytes, optional, 0x20b);
	put32(&mut bytes, optional + 60, 0x200);
	put32(&mut bytes, optional + 108, 1);
	put32(&mut bytes, optional + 112, 0x1000);
	put32(&mut bytes, optional + 116, 0x800);
	let section = optional + 240;
	put32(&mut bytes, section + 8, 0x1000);
	put32(&mut bytes, section + 12, 0x1000);
	put32(&mut bytes, section + 16, 0x1000);
	put32(&mut bytes, section + 20, 0x200);
	put32(&mut bytes, section + 36, 0x60000020);
	let export = 0x200;
	put32(&mut bytes, export + 16, 1);
	put32(&mut bytes, export + 20, names.len() as u32);
	put32(&mut bytes, export + 24, names.len() as u32);
	put32(&mut bytes, export + 28, 0x1040);
	put32(&mut bytes, export + 32, 0x10c0);
	put32(&mut bytes, export + 36, 0x1140);
	let mut text = 0x400;
	for (index, name) in names.iter().enumerate() {
		put32(&mut bytes, 0x240 + index * 4, 0x1900 + index as u32);
		put32(
			&mut bytes,
			0x2c0 + index * 4,
			(text - 0x200 + 0x1000) as u32,
		);
		put16(&mut bytes, 0x340 + index * 2, index as u16);
		bytes[text..text + name.len()].copy_from_slice(name.as_bytes());
		text += name.len() + 1;
	}
	bytes
}

#[test]
fn mixed_graph_plugin_is_refused_and_distinct_host_plugin_artifacts_validate() {
	let plugin = dll(&["foch_plugin_get_api"]);
	let host_bytes = dll(&PROXY_EXPORTS);
	assert!(host::validate_plugin(&plugin).is_ok());
	assert!(host::validate_host(&host_bytes).is_ok());
	assert!(host::validate_plugin(&host_bytes).is_err());
	assert!(host::validate_host(&plugin).is_err());
	let mut mixed = PROXY_EXPORTS.to_vec();
	mixed.push("foch_plugin_get_api");
	assert!(host::validate_plugin(&dll(&mixed)).is_err());
	assert!(host::validate_host(&dll(&mixed)).is_err());
}

#[test]
fn truncated_malformed_duplicate_and_forwarded_exports_are_refused() {
	let complete = dll(&["foch_plugin_get_api"]);
	for end in 0..complete.len() {
		assert!(host::validate_plugin(&complete[..end]).is_err(), "{end}");
	}
	assert!(host::validate_plugin(&dll(&["foch_plugin_get_api", "foch_plugin_get_api"])).is_err());
	for (offset, value) in [(0x240, 0x1100), (0x240, 0xffff_ff00), (0x2c0, 0xffff_ff00)] {
		let mut corrupt = complete.clone();
		put32(&mut corrupt, offset, value);
		assert!(host::validate_plugin(&corrupt).is_err());
	}
}

#[test]
fn resolution_preserves_distribution_name_and_supports_distinct_development_artifact() {
	let directory = tempfile::tempdir().unwrap();
	assert!(
		host::resolve(directory.path(), None)
			.unwrap_err()
			.to_string()
			.contains("foch-runtime-host")
	);
	std::fs::write(directory.path().join(host::BUILT_DLL), b"built").unwrap();
	assert_eq!(
		host::resolve(directory.path(), None).unwrap(),
		directory.path().join(host::BUILT_DLL)
	);
	std::fs::write(directory.path().join(host::DISTRIBUTED_DLL), b"distributed").unwrap();
	assert_eq!(
		host::resolve(directory.path(), None).unwrap(),
		directory.path().join(host::BUILT_DLL)
	);
	std::fs::remove_file(directory.path().join(host::BUILT_DLL)).unwrap();
	assert_eq!(
		host::resolve(directory.path(), None).unwrap(),
		directory.path().join(host::DISTRIBUTED_DLL)
	);
	assert!(host::resolve(directory.path(), Some(Path::new("absent.dll"))).is_err());
}
