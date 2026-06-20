#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtSchema {
	pub types: Vec<CwtType>,
	pub enums: Vec<CwtEnum>,
	pub aliases: Vec<CwtAlias>,
	pub scopes: Vec<CwtScope>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtType {
	pub name: String,
	pub path: Option<String>,
	pub name_field: Option<String>,
	pub name_from_file: bool,
	pub type_per_file: bool,
	pub skip_root_key: Vec<String>,
	pub subtypes: Vec<CwtSubtype>,
	pub options: Vec<CwtOption>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtSubtype {
	pub name: String,
	pub type_key_filter: Vec<String>,
	pub options: Vec<CwtOption>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtEnum {
	pub name: String,
	pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtAlias {
	pub category: String,
	pub name: String,
	pub scope: Vec<String>,
	pub options: Vec<CwtOption>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtScope {
	pub name: String,
	pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtOption {
	pub key: String,
	pub value: String,
}
