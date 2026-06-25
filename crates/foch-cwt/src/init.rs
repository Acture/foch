use crate::schema::CwtSchemaGraph;
use foch_core::model::base_scope;

pub fn install_base_scopes(graph: &CwtSchemaGraph) {
	if base_scope::is_initialized() {
		return;
	}
	let country = graph
		.scope_definitions()
		.iter()
		.find_map(|scope| match scope.aliases.as_slice() {
			[alias, ..] if alias == "country" => Some(alias.as_str()),
			_ => None,
		});
	let province = graph
		.scope_definitions()
		.iter()
		.find_map(|scope| match scope.aliases.as_slice() {
			[alias, ..] if alias == "province" => Some(alias.as_str()),
			_ => None,
		});
	if let (Some(country), Some(province)) = (country, province) {
		base_scope::init_base_scopes(country, province);
	}
}
