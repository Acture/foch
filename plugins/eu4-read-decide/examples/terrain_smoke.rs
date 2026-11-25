//! One-shot smoke against the installed game's terrain file:
//! `EU4_ROOT=G:/.../Europa Universalis IV cargo run -p foch-plugin-read-decide --example terrain_smoke`
fn main() {
	let Some(root) = std::env::var_os("EU4_ROOT") else {
		eprintln!("EU4_ROOT not set; nothing to do");
		return;
	};
	let path = std::path::Path::new(&root).join("map/terrain.txt");
	let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
	let mut codec = foch_plugin_read_decide::terrain::TerrainCodec::from_sources(&[text]);
	for name in [
		"hills",
		"mountains",
		"marsh",
		"forest",
		"plains",
		"farmlands",
		"steppe",
		"grasslands",
		"hills_forest",
		"savannah",
		"coastline",
		"urban",
	] {
		println!("{name:14} -> bucket {}", codec.bucket(name));
	}
	for (name, count) in codec.drain_unknown() {
		println!("UNKNOWN {name} x{count} -> plains fallback");
	}
}
