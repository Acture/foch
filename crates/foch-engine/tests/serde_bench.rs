use std::collections::HashMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
struct BenchScopeNode {
	id: usize,
	kind: u8,
	parent: Option<usize>,
	this_type: u8,
	aliases: HashMap<String, u8>,
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
struct BenchSymbolDef {
	kind: u8,
	name: String,
	module: String,
	local_name: String,
	mod_id: String,
	path: String,
	line: usize,
	column: usize,
	scope_id: usize,
	params: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug))]
struct BenchPayload {
	scopes: Vec<BenchScopeNode>,
	definitions: Vec<BenchSymbolDef>,
}

fn generate_test_data() -> BenchPayload {
	let mut scopes = Vec::with_capacity(640_000);
	let mut definitions = Vec::with_capacity(10_000);

	for i in 0..640_000 {
		let mut aliases = HashMap::new();
		if i % 10 == 0 {
			aliases.insert("THIS".to_string(), 0);
			aliases.insert("ROOT".to_string(), 0);
		}
		scopes.push(BenchScopeNode {
			id: i,
			kind: (i % 5) as u8,
			parent: if i > 0 { Some(i - 1) } else { None },
			this_type: (i % 3) as u8,
			aliases,
			mod_id: format!("mod_{}", i % 10),
			path: format!("common/ideas/idea_file_{}.txt", i % 100),
			line: i % 1000,
			column: i % 80,
		});
	}

	for i in 0..10_000 {
		definitions.push(BenchSymbolDef {
			kind: (i % 6) as u8,
			name: format!("symbol_{}", i),
			module: format!("module_{}", i % 50),
			local_name: format!("local_{}", i),
			mod_id: format!("mod_{}", i % 10),
			path: format!("common/scripted_effects/effects_{}.txt", i % 20),
			line: i % 500,
			column: i % 40,
			scope_id: i % 640_000,
			params: if i % 5 == 0 {
				vec!["param1".into(), "param2".into()]
			} else {
				vec![]
			},
		});
	}

	BenchPayload { scopes, definitions }
}

/// Serialization benchmark comparing bincode 1.x, bincode 2.x, bitcode, postcard,
/// and rkyv on a realistic SymbolScope-like payload (640k scopes + 10k definitions).
///
/// Run with: cargo test -p foch-engine benchmark_serde_libraries -- --ignored --nocapture
/// For realistic timings use release: cargo test -p foch-engine --release benchmark_serde_libraries -- --ignored --nocapture
#[test]
#[ignore]
fn benchmark_serde_libraries() {
	let data = generate_test_data();
	println!(
		"\nGenerated {} scopes + {} definitions",
		data.scopes.len(),
		data.definitions.len()
	);

	// bincode 1.x (current workspace dependency)
	{
		let start = Instant::now();
		let bytes = bincode::serialize(&data).unwrap();
		let ser_time = start.elapsed();

		let start = Instant::now();
		let _: BenchPayload = bincode::deserialize(&bytes).unwrap();
		let deser_time = start.elapsed();

		println!(
			"bincode 1.x:  ser={:?}  deser={:?}  size={:.2}MB",
			ser_time,
			deser_time,
			bytes.len() as f64 / 1_000_000.0,
		);
	}

	// bincode 2.0.0-rc.3 (via serde compat layer)
	{
		let config = bincode2::config::standard();

		let start = Instant::now();
		let bytes = bincode2::serde::encode_to_vec(&data, config).unwrap();
		let ser_time = start.elapsed();

		let start = Instant::now();
		let (_result, _len): (BenchPayload, _) =
			bincode2::serde::decode_from_slice(&bytes, config).unwrap();
		let deser_time = start.elapsed();

		println!(
			"bincode 2.x:  ser={:?}  deser={:?}  size={:.2}MB",
			ser_time,
			deser_time,
			bytes.len() as f64 / 1_000_000.0,
		);
	}

	// bitcode 0.6
	{
		let start = Instant::now();
		let bytes = bitcode::serialize(&data).unwrap();
		let ser_time = start.elapsed();

		let start = Instant::now();
		let _: BenchPayload = bitcode::deserialize(&bytes).unwrap();
		let deser_time = start.elapsed();

		println!(
			"bitcode:      ser={:?}  deser={:?}  size={:.2}MB",
			ser_time,
			deser_time,
			bytes.len() as f64 / 1_000_000.0,
		);
	}

	// postcard 1.x
	{
		let start = Instant::now();
		let bytes = postcard::to_allocvec(&data).unwrap();
		let ser_time = start.elapsed();

		let start = Instant::now();
		let _: BenchPayload = postcard::from_bytes(&bytes).unwrap();
		let deser_time = start.elapsed();

		println!(
			"postcard:     ser={:?}  deser={:?}  size={:.2}MB",
			ser_time,
			deser_time,
			bytes.len() as f64 / 1_000_000.0,
		);
	}

	// rkyv 0.8 — full deserialize-to-owned
	{
		let start = Instant::now();
		let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&data).unwrap();
		let ser_time = start.elapsed();

		let start = Instant::now();
		let _: BenchPayload =
			rkyv::from_bytes::<BenchPayload, rkyv::rancor::Error>(&bytes).unwrap();
		let deser_time = start.elapsed();

		println!(
			"rkyv (owned): ser={:?}  deser={:?}  size={:.2}MB",
			ser_time,
			deser_time,
			bytes.len() as f64 / 1_000_000.0,
		);
	}

	// rkyv 0.8 — zero-copy access (just validates + casts, no allocation)
	{
		let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&data).unwrap();

		let start = Instant::now();
		let archived =
			rkyv::access::<ArchivedBenchPayload, rkyv::rancor::Error>(&bytes).unwrap();
		let access_time = start.elapsed();

		println!(
			"rkyv (zero-copy access): {:?}  scopes[0].id={}",
			access_time, archived.scopes[0].id,
		);
	}
}
