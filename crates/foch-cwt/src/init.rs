use crate::schema::CwtSchemaGraph;

pub fn install_base_scopes(graph: &CwtSchemaGraph) {
	let _ = graph
		.scope_definitions()
		.iter()
		.find_map(|scope| match scope.aliases.as_slice() {
			[alias, ..] if alias == "country" => Some(alias),
			_ => None,
		});
	let _ = graph
		.scope_definitions()
		.iter()
		.find_map(|scope| match scope.aliases.as_slice() {
			[alias, ..] if alias == "province" => Some(alias),
			_ => None,
		});
	// TODO(T3): call foch_core::model::base_scope::init_base_scopes(country_name, province_name)
	// once the scope registry lands in codex/foch-b0.
}
