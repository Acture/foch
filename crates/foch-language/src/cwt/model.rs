#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtSchema {
	pub types: Vec<CwtType>,
	pub enums: Vec<CwtEnum>,
	pub value_sets: Vec<CwtValueSet>,
	pub aliases: Vec<CwtAlias>,
	pub single_aliases: Vec<CwtSingleAlias>,
	pub complex_enums: Vec<CwtComplexEnum>,
	pub scopes: Vec<CwtScope>,
	pub links: Vec<CwtLink>,
	pub rule_bodies: Vec<CwtRuleBodyEntry>,
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
	pub rules: Vec<CwtRule>,
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
pub struct CwtValueSet {
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
pub struct CwtSingleAlias {
	pub name: String,
	pub rules: Vec<CwtRule>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtComplexEnum {
	pub name: String,
	pub path: Option<String>,
	pub start_from_root: bool,
	pub name_rules: Vec<CwtRule>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtScope {
	pub name: String,
	pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtLink {
	pub name: String,
	pub input_scopes: Vec<String>,
	pub output_scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtOption {
	pub key: String,
	pub value: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtRuleBodyEntry {
	pub key: String,
	pub rules: Vec<CwtRule>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CwtValueType {
	Scalar,
	Bool,
	Int(Option<CwtRange>),
	Float(Option<CwtRange>),
	Percentage,
	DateField,
	Localisation,
	Filepath,
	Colour,
	TypeRef(String),
	Enum(String),
	Value(String),
	ValueSet(String),
	Scope(String),
	AliasName(String),
	AliasMatchLeft(String),
	SingleAliasRight(String),
	Literal(String),
	Unknown(String),
}

impl CwtValueType {
	pub fn from_token(token: &str) -> Self {
		let token = token.trim();
		match token {
			"scalar" => Self::Scalar,
			"bool" => Self::Bool,
			"int" => Self::Int(None),
			"float" => Self::Float(None),
			"percentage_field" => Self::Percentage,
			"date_field" => Self::DateField,
			"localisation" => Self::Localisation,
			"filepath" => Self::Filepath,
			"colour" | "color" => Self::Colour,
			_ => Self::from_structured_token(token),
		}
	}

	fn from_structured_token(token: &str) -> Self {
		if let Some(inner) = token
			.strip_prefix('<')
			.and_then(|value| value.strip_suffix('>'))
			.map(str::trim)
			.filter(|value| !value.is_empty())
		{
			return Self::TypeRef(inner.to_string());
		}

		if let Some((head, inner)) = parse_bracket_key(token) {
			return match head {
				"enum" => Self::Enum(inner.to_string()),
				"value" => Self::Value(inner.to_string()),
				"value_set" => Self::ValueSet(inner.to_string()),
				"scope" => Self::Scope(inner.to_string()),
				"alias_name" => Self::AliasName(inner.to_string()),
				"alias_match_left" => Self::AliasMatchLeft(inner.to_string()),
				"single_alias_right" => Self::SingleAliasRight(inner.to_string()),
				"int" => Self::Int(parse_range(inner)),
				"float" => Self::Float(parse_range(inner)),
				_ => Self::Unknown(token.to_string()),
			};
		}

		if token.contains('[') || token.contains(']') || token.is_empty() {
			Self::Unknown(token.to_string())
		} else {
			Self::Literal(token.to_string())
		}
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct CwtRange {
	pub min: String,
	pub max: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CwtRuleBody {
	Leaf(CwtValueType),
	Block(Vec<CwtRule>),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CwtRule {
	pub key: String,
	pub body: CwtRuleBody,
	pub cardinality: Option<String>,
	pub options: Vec<CwtOption>,
}

impl Default for CwtRuleBody {
	fn default() -> Self {
		Self::Block(Vec::new())
	}
}

fn parse_range(value: &str) -> Option<CwtRange> {
	let (min, max) = value.split_once("..")?;
	let min = min.trim();
	let max = max.trim();
	if min.is_empty() || max.is_empty() {
		return None;
	}

	Some(CwtRange {
		min: min.to_string(),
		max: max.to_string(),
	})
}

pub fn parse_bracket_key(key: &str) -> Option<(&str, &str)> {
	let open = key.find('[')?;
	let close = key.rfind(']')?;
	if close + 1 != key.len() {
		return None;
	}

	let head = key[..open].trim();
	let inner = key[open + 1..close].trim();
	if head.is_empty() || inner.is_empty() {
		return None;
	}

	Some((head, inner))
}
