use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::iter::FromIterator;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign};
use std::sync::{OnceLock, RwLock};

#[derive(
	Clone,
	Copy,
	Debug,
	Eq,
	PartialEq,
	Hash,
	Serialize,
	Deserialize,
	rkyv::Archive,
	rkyv::Serialize,
	rkyv::Deserialize,
)]
pub struct ScopeType(u16);

impl ScopeType {
	pub fn name(self) -> &'static str {
		scope_name(self)
	}

	pub const fn as_index(self) -> u16 {
		self.0
	}
}

#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Eq,
	PartialEq,
	Hash,
	Serialize,
	Deserialize,
	rkyv::Archive,
	rkyv::Serialize,
	rkyv::Deserialize,
)]
pub enum MaybeScope {
	Known(ScopeType),
	#[default]
	Unknown,
}

impl MaybeScope {
	pub const fn known(scope_type: ScopeType) -> Self {
		Self::Known(scope_type)
	}

	pub const fn as_known(self) -> Option<ScopeType> {
		match self {
			Self::Known(scope_type) => Some(scope_type),
			Self::Unknown => None,
		}
	}

	pub const fn is_unknown(self) -> bool {
		matches!(self, Self::Unknown)
	}
}

impl From<ScopeType> for MaybeScope {
	fn from(value: ScopeType) -> Self {
		Self::Known(value)
	}
}

impl From<Option<ScopeType>> for MaybeScope {
	fn from(value: Option<ScopeType>) -> Self {
		value.map_or(Self::Unknown, Self::Known)
	}
}

#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Eq,
	PartialEq,
	Hash,
	Serialize,
	Deserialize,
	rkyv::Archive,
	rkyv::Serialize,
	rkyv::Deserialize,
)]
pub struct ScopeSet(u128);

impl ScopeSet {
	pub const EMPTY: Self = Self(0);
	pub const ALL: Self = Self(u128::MAX);

	pub fn from_scopes<I>(iter: I) -> Self
	where
		I: IntoIterator<Item = ScopeType>,
	{
		iter.into_iter().collect()
	}

	pub fn contains(self, scope_type: ScopeType) -> bool {
		(self.0 & bit_for(scope_type)) != 0
	}

	pub fn insert(&mut self, scope_type: ScopeType) {
		self.0 |= bit_for(scope_type);
	}

	pub const fn is_empty(self) -> bool {
		self.0 == 0
	}

	pub const fn union(self, other: Self) -> Self {
		Self(self.0 | other.0)
	}

	pub const fn intersect(self, other: Self) -> Self {
		Self(self.0 & other.0)
	}

	pub fn as_single_scope(self) -> Option<ScopeType> {
		(self.0.count_ones() == 1).then_some(ScopeType(self.0.trailing_zeros() as u16))
	}
}

impl FromIterator<ScopeType> for ScopeSet {
	fn from_iter<T: IntoIterator<Item = ScopeType>>(iter: T) -> Self {
		let mut set = Self::EMPTY;
		for scope_type in iter {
			set.insert(scope_type);
		}
		set
	}
}

impl From<ScopeType> for ScopeSet {
	fn from(value: ScopeType) -> Self {
		let mut set = Self::EMPTY;
		set.insert(value);
		set
	}
}

impl From<MaybeScope> for ScopeSet {
	fn from(value: MaybeScope) -> Self {
		value.as_known().map_or(Self::EMPTY, Self::from)
	}
}

impl BitOr for ScopeSet {
	type Output = Self;

	fn bitor(self, rhs: Self) -> Self::Output {
		self.union(rhs)
	}
}

impl BitOrAssign for ScopeSet {
	fn bitor_assign(&mut self, rhs: Self) {
		self.0 |= rhs.0;
	}
}

impl BitAnd for ScopeSet {
	type Output = Self;

	fn bitand(self, rhs: Self) -> Self::Output {
		self.intersect(rhs)
	}
}

impl BitAndAssign for ScopeSet {
	fn bitand_assign(&mut self, rhs: Self) {
		self.0 &= rhs.0;
	}
}

#[derive(Debug, Default)]
pub struct ScopeRegistry {
	names: Vec<&'static str>,
	by_name: HashMap<String, ScopeType>,
}

pub static GLOBAL_REGISTRY: OnceLock<RwLock<ScopeRegistry>> = OnceLock::new();

#[derive(Clone, Copy, Debug)]
struct BaseScopes {
	country: ScopeType,
	province: ScopeType,
}

static BASE_SCOPES: OnceLock<RwLock<Option<BaseScopes>>> = OnceLock::new();

impl ScopeRegistry {
	#[cfg(any(test, feature = "test-utils"))]
	pub fn install_test_defaults() {
		base_scope::init_base_scopes("country", "province");
	}
}

pub fn intern_scope(name: &str) -> ScopeType {
	let registry = registry();
	let mut registry = registry
		.write()
		.unwrap_or_else(|poisoned| poisoned.into_inner());
	if let Some(scope_type) = registry.by_name.get(name).copied() {
		return scope_type;
	}
	let leaked = Box::leak(name.to_string().into_boxed_str());
	let scope_type = ScopeType(registry.names.len() as u16);
	registry.names.push(leaked);
	registry.by_name.insert(leaked.to_string(), scope_type);
	scope_type
}

pub fn lookup_scope(name: &str) -> Option<ScopeType> {
	registry()
		.read()
		.unwrap_or_else(|poisoned| poisoned.into_inner())
		.by_name
		.get(name)
		.copied()
}

pub fn scope_name(scope_type: ScopeType) -> &'static str {
	let registry = registry()
		.read()
		.unwrap_or_else(|poisoned| poisoned.into_inner());
	registry
		.names
		.get(scope_type.as_index() as usize)
		.unwrap_or_else(|| panic!("scope index {} is not registered", scope_type.as_index()))
}

pub mod base_scope {
	use super::{BASE_SCOPES, BaseScopes, ScopeType, intern_scope};
	use std::sync::RwLock;

	pub fn country() -> ScopeType {
		base_scopes().country
	}

	pub fn province() -> ScopeType {
		base_scopes().province
	}

	pub fn is_initialized() -> bool {
		state()
			.read()
			.unwrap_or_else(|poisoned| poisoned.into_inner())
			.is_some()
	}

	pub fn init_base_scopes(country_name: &str, province_name: &str) {
		let mut state = state()
			.write()
			.unwrap_or_else(|poisoned| poisoned.into_inner());
		*state = Some(BaseScopes {
			country: intern_scope(country_name),
			province: intern_scope(province_name),
		});
	}

	#[cfg(test)]
	pub(crate) fn reset_for_tests() {
		let mut state = state()
			.write()
			.unwrap_or_else(|poisoned| poisoned.into_inner());
		*state = None;
	}

	fn state() -> &'static RwLock<Option<BaseScopes>> {
		BASE_SCOPES.get_or_init(|| RwLock::new(None))
	}

	fn base_scopes() -> BaseScopes {
		state()
			.read()
			.unwrap_or_else(|poisoned| poisoned.into_inner())
			.as_ref()
			.copied()
			.expect("base scopes are not initialized")
	}
}

fn registry() -> &'static RwLock<ScopeRegistry> {
	GLOBAL_REGISTRY.get_or_init(|| RwLock::new(ScopeRegistry::default()))
}

fn bit_for(scope_type: ScopeType) -> u128 {
	let index = scope_type.as_index() as u32;
	assert!(index < 128, "scope index {index} exceeds ScopeSet capacity");
	1_u128 << index
}

#[cfg(any(test, feature = "test-utils"))]
pub mod test_support {
	use super::ScopeRegistry;

	pub fn install_defaults() {
		ScopeRegistry::install_test_defaults();
	}
}

#[cfg(test)]
mod tests {
	use super::{MaybeScope, ScopeRegistry, ScopeSet, base_scope, intern_scope, scope_name};

	#[test]
	fn intern_scope_returns_same_scope_type_for_same_name() {
		let first = intern_scope("foo");
		let second = intern_scope("foo");
		assert_eq!(first, second);
	}

	#[test]
	fn scope_name_returns_registered_name() {
		let bar = intern_scope("bar");
		assert_eq!(scope_name(bar), "bar");
	}

	#[test]
	fn empty_scope_set_never_contains_scopes() {
		let any = intern_scope("baz");
		assert!(!ScopeSet::EMPTY.contains(any));
	}

	#[test]
	fn scope_set_from_iter_contains_inserted_scopes() {
		ScopeRegistry::install_test_defaults();
		let set = ScopeSet::from_scopes([base_scope::country(), base_scope::province()]);
		assert!(set.contains(base_scope::country()));
	}

	#[test]
	fn maybe_scope_known_returns_inner_scope() {
		let scope_type = intern_scope("qux");
		assert_eq!(MaybeScope::Known(scope_type).as_known(), Some(scope_type));
	}

	#[test]
	fn maybe_scope_unknown_reports_unknown() {
		assert!(MaybeScope::Unknown.is_unknown());
	}

	#[test]
	fn rkyv_round_trip_preserves_scope_types() {
		ScopeRegistry::install_test_defaults();
		let scope_type = base_scope::country();
		let scope_set = ScopeSet::from_scopes([base_scope::country(), base_scope::province()]);
		let maybe_scope = MaybeScope::Known(base_scope::province());

		let scope_type_bytes =
			rkyv::to_bytes::<rkyv::rancor::Error>(&scope_type).expect("encode scope type");
		let scope_set_bytes =
			rkyv::to_bytes::<rkyv::rancor::Error>(&scope_set).expect("encode scope set");
		let maybe_scope_bytes =
			rkyv::to_bytes::<rkyv::rancor::Error>(&maybe_scope).expect("encode maybe scope");

		let decoded_scope_type =
			rkyv::from_bytes::<super::ScopeType, rkyv::rancor::Error>(&scope_type_bytes)
				.expect("decode scope type");
		let decoded_scope_set = rkyv::from_bytes::<ScopeSet, rkyv::rancor::Error>(&scope_set_bytes)
			.expect("decode scope set");
		let decoded_maybe_scope =
			rkyv::from_bytes::<MaybeScope, rkyv::rancor::Error>(&maybe_scope_bytes)
				.expect("decode maybe scope");

		assert_eq!(decoded_scope_type, scope_type);
		assert_eq!(decoded_scope_set, scope_set);
		assert_eq!(decoded_maybe_scope, maybe_scope);
	}

	#[test]
	fn base_scope_initialization_state_changes() {
		base_scope::reset_for_tests();
		assert!(!base_scope::is_initialized());
		ScopeRegistry::install_test_defaults();
		assert!(base_scope::is_initialized());
	}
}
