//! Why a resource reference does not resolve.
//!
//! A name nothing declares is not automatically a mistake. The file that
//! would declare it may have failed to parse, the mod that declares it may
//! be missing from the playset, or a later mod may have replaced the
//! directory it lives in. Each cause wants a different fix, so the analysis
//! tells them apart instead of reporting every unresolved name as "not
//! found".
//!
//! The classification needs the playset, not the file alone: a name is only
//! missing when every file that could declare it was read. A file that
//! failed to parse, a base game that is not part of the input, and a family
//! the rules do not describe all leave the answer unknown, and an unknown
//! answer is never reported as a missing name.

use std::collections::{HashMap, HashSet};

use crate::game::eu4::content::eu4;
use crate::game::eu4::script::{
	EVENT_NAMESPACE_KEY, SPRITE_DEFINITION_KEY, SPRITE_REFERENCE_KEY, TYPED_REFERENCE_KEY,
};
use crate::model::{Finding, FindingChannel, GamePathBuf, SemanticIndex, Severity, SymbolKind};

/// A place that declares a name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationSite {
	/// The mod that declares it. The base game's own id stands for the base
	/// game.
	pub mod_id: String,
	/// The file, relative to the mod's root.
	pub path: GamePathBuf,
	pub line: usize,
	pub column: usize,
}

/// The kind of definition a reference names.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ReferenceKind {
	/// An event, by its id.
	Event,
	/// An event namespace, by name.
	Namespace,
	/// A definition of a content family, by the key the family's
	/// declarations are recorded under.
	Family(String),
	/// A definition of a schema type, by the type's name.
	Type(String),
	/// A GUI sprite, by name.
	Sprite,
}

impl ReferenceKind {
	/// The kind's name, as a diagnostic reads it.
	pub fn as_str(&self) -> &str {
		match self {
			Self::Event => "event",
			Self::Namespace => "event_namespace",
			Self::Family(key) => key,
			Self::Type(name) => name,
			Self::Sprite => "sprite",
		}
	}
}

/// Why a reference does not resolve.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReferenceFailure {
	/// No file of the effective playset declares the name.
	NotDeclared,
	/// A file that could declare the name did not parse, so whether it
	/// declares the name is unknown.
	BrokenSource { file: DeclarationSite },
	/// The name is declared, but the effective playset keeps none of the
	/// declarations: a later mod replaced the directory they live in.
	Replaced { shadowed: Vec<DeclarationSite> },
	/// The referring mod depends on a mod the playset does not load, so the
	/// name may well be declared there.
	MissingDependency { dependency: String },
	/// The sprite is declared, but the texture it names is not in the
	/// playset.
	MissingTexture { texture: GamePathBuf },
	/// The sprite is declared, but names no texture: the engine has nothing
	/// to load.
	UnloadableSprite,
	/// The analysis cannot tell.
	Unknown { reason: UnknownReason },
}

/// Why the analysis cannot tell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnknownReason {
	/// The rules in force do not describe this kind of reference.
	NoRule,
	/// The scope searched does not cover every file that could declare the
	/// name.
	IncompleteScope,
}

impl UnknownReason {
	/// The reason's name, as a diagnostic reads it.
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::NoRule => "no_rule",
			Self::IncompleteScope => "incomplete_scope",
		}
	}
}

/// What the analysis found about a reference that does not resolve.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceDiagnosis {
	/// What the reference names.
	pub kind: ReferenceKind,
	/// The name that does not resolve.
	pub name: String,
	/// Why it does not resolve.
	pub failure: ReferenceFailure,
	/// Every declaration of the name the analysis found. An empty list means
	/// nothing declares the name.
	pub providers: Vec<DeclarationSite>,
}

/// The rule a failure is reported under.
pub fn failure_rule_id(failure: &ReferenceFailure) -> &'static str {
	match failure {
		ReferenceFailure::NotDeclared => "unresolved-reference-not-declared",
		ReferenceFailure::BrokenSource { .. } => "unresolved-reference-broken-source",
		ReferenceFailure::Replaced { .. } => "unresolved-reference-replaced",
		ReferenceFailure::MissingDependency { .. } => "unresolved-reference-missing-dependency",
		ReferenceFailure::MissingTexture { .. } => "unresolved-reference-missing-texture",
		ReferenceFailure::UnloadableSprite => "unresolved-reference-unloadable-sprite",
		ReferenceFailure::Unknown { .. } => "unresolved-reference-unknown",
	}
}

/// The severity and channel a failure is reported with.
///
/// Only a failure the analysis is sure of is strict: a missing name and a
/// replaced definition are, while a broken source, a missing dependency and
/// an unusable sprite are leads worth reading rather than verdicts.
fn failure_severity(failure: &ReferenceFailure) -> (Severity, FindingChannel) {
	match failure {
		ReferenceFailure::NotDeclared | ReferenceFailure::Replaced { .. } => {
			(Severity::Error, FindingChannel::Strict)
		}
		ReferenceFailure::MissingTexture { .. } => (Severity::Error, FindingChannel::Strict),
		ReferenceFailure::BrokenSource { .. }
		| ReferenceFailure::MissingDependency { .. }
		| ReferenceFailure::UnloadableSprite => (Severity::Warning, FindingChannel::Advisory),
		ReferenceFailure::Unknown { .. } => (Severity::Info, FindingChannel::Advisory),
	}
}

/// The playset facts a diagnosis needs.
#[derive(Clone, Debug, Default)]
pub struct PlaysetFacts {
	/// A mod's precedence. A larger precedence loads later.
	precedence: HashMap<String, usize>,
	/// The directories the loaded mods replace, by the precedence of the
	/// mod that replaces them.
	///
	/// Only a mod that loads later can replace a declaration away, so the
	/// directory's precedence answers whether a declaration survives
	/// without a scan over every mod.
	replaced: HashMap<GamePathBuf, usize>,
	/// The dependencies of a mod the playset does not load, by the mod.
	missing_dependencies: HashMap<String, Vec<String>>,
	/// The mods the playset loads.
	loaded: HashSet<String>,
}

/// The dependencies of a mod the playset does not load.
///
/// A dependency names a mod the way the launcher reads it: by its display
/// name or its workshop id. The token is resolved against the playset's
/// identities first, so a dependency on a mod that sits in the playset
/// disabled is missing rather than satisfied.
fn unloaded_dependencies(
	identity: &crate::playset::dependency::ModIdentityIndex,
	mods: &[crate::model::ModCandidate],
	dependencies: &[String],
) -> Vec<String> {
	dependencies
		.iter()
		.filter(|dependency| match identity.lookup(dependency) {
			None => true,
			Some(index) => !mods[index].entry.enabled,
		})
		.cloned()
		.collect()
}

impl PlaysetFacts {
	/// The facts of a playset: the mods it loads, in load order, and what
	/// each of them replaces and depends on.
	pub fn from_mods(mods: &[crate::model::ModCandidate]) -> Self {
		let identity = crate::playset::dependency::ModIdentityIndex::from_mods(mods);
		let mut facts = Self::default();
		for (index, mod_item) in mods.iter().enumerate() {
			if !mod_item.entry.enabled {
				continue;
			}
			let precedence = index + 1;
			facts.precedence.insert(mod_item.mod_id.clone(), precedence);
			facts.loaded.insert(mod_item.mod_id.clone());
			let Some(descriptor) = mod_item.descriptor.as_ref() else {
				continue;
			};
			for directory in &descriptor.replace_path {
				facts
					.replaced
					.entry(directory.clone())
					.and_modify(|later| *later = (*later).max(precedence))
					.or_insert(precedence);
			}
			let missing = unloaded_dependencies(&identity, mods, &descriptor.dependencies);
			if !missing.is_empty() {
				facts
					.missing_dependencies
					.insert(mod_item.mod_id.clone(), missing);
			}
		}
		facts
	}

	/// Whether the mod's declarations are part of the effective playset.
	pub fn loads(&self, mod_id: &str) -> bool {
		self.loaded.contains(mod_id)
	}

	/// Whether the declaration at `site` is part of the effective playset.
	///
	/// A declaration is part of it when its mod is loaded and no mod that
	/// loads later replaces the directory the declaration lives in: a
	/// `replace_path` starts a directory fresh, so what an earlier mod
	/// declared there is gone.
	pub fn is_effective(&self, site: &DeclarationSite) -> bool {
		if !self.mod_is_loaded(&site.mod_id) {
			return false;
		}
		let precedence = self.precedence.get(&site.mod_id).copied().unwrap_or(0);
		// The directories that can hold the declaration: the file's own
		// path and every directory above it. A directory the playset
		// replaces takes what an earlier mod declared there away.
		let mut directory = Some(site.path.as_game_path());
		while let Some(current) = directory {
			if self
				.replaced
				.get(current)
				.is_some_and(|later| *later > precedence)
			{
				return false;
			}
			directory = current.parent();
		}
		true
	}

	/// Whether the mod's declarations are part of the effective playset.
	///
	/// The base game is not a mod of the playset, but the game is always
	/// loaded, before every mod.
	fn mod_is_loaded(&self, mod_id: &str) -> bool {
		self.loaded.contains(mod_id)
			|| crate::game::eu4::base::snapshot::is_base_game_mod_id(mod_id)
	}

	/// The dependencies of `mod_id` the playset does not load: the ones no
	/// mod answers to, and the ones only a mod the playset does not load
	/// answers to.
	pub fn missing_dependencies(&self, mod_id: &str) -> &[String] {
		self.missing_dependencies
			.get(mod_id)
			.map_or(&[], Vec::as_slice)
	}
}

/// A sprite declaration: the sprite's name, and the texture it names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpriteDeclaration {
	/// Where the sprite is declared.
	pub site: DeclarationSite,
	/// The texture file the sprite loads, when it names one.
	pub texture: Option<GamePathBuf>,
}

/// The declarations the index holds, by kind and name.
#[derive(Default)]
pub struct DeclarationIndex {
	/// The declarations of an event, by its full id and by its bare id.
	events: HashMap<String, Vec<DeclarationSite>>,
	/// The declarations of an event, by the id the event file writes.
	event_ids: HashMap<String, Vec<DeclarationSite>>,
	/// The namespaces the event files declare.
	namespaces: HashMap<String, Vec<DeclarationSite>>,
	/// The declarations of a content family, by the key the family records
	/// them under and by the name declared.
	families: HashMap<String, HashMap<String, Vec<DeclarationSite>>>,
	/// The content family a family's declarations live in.
	family_directories: HashMap<String, &'static str>,
	/// The sprite declarations, by the sprite's name.
	sprites: HashMap<String, Vec<SpriteDeclaration>>,
	/// The files that did not parse, by the content family they belong to.
	broken_files: HashMap<String, Vec<DeclarationSite>>,
}

impl DeclarationIndex {
	/// The declarations the index holds.
	pub fn from_index(index: &SemanticIndex) -> Self {
		let mut declarations = Self::default();
		for definition in &index.definitions {
			let site = DeclarationSite {
				mod_id: definition.mod_id.clone(),
				path: definition.path.clone(),
				line: definition.line,
				column: definition.column,
			};
			if definition.kind == SymbolKind::Event {
				declarations
					.event_ids
					.entry(definition.name.clone())
					.or_default()
					.push(site.clone());
				declarations.push_event(&definition.name, site.clone());
				// The engine resolves a bare id against the tail of a
				// qualified one, so the tail is a name the event answers
				// to as well.
				if let Some((_, tail)) = definition.name.rsplit_once('.') {
					declarations.push_event(tail, site);
				}
			}
		}
		for resource in &index.resource_references {
			let site = DeclarationSite {
				mod_id: resource.mod_id.clone(),
				path: resource.path.clone(),
				line: resource.line,
				column: resource.column,
			};
			if let Some(name) = resource.key.strip_prefix(SPRITE_DEFINITION_KEY) {
				declarations
					.sprites
					.entry(name.to_string())
					.or_default()
					.push(SpriteDeclaration {
						site,
						texture: GamePathBuf::parse(&resource.value).ok(),
					});
				continue;
			}
			match resource.key.as_str() {
				EVENT_NAMESPACE_KEY => {
					declarations
						.namespaces
						.entry(resource.value.clone())
						.or_default()
						.push(site);
				}
				key if key.ends_with("_definition") => {
					let Some(family) = definition_reference_family(key) else {
						continue;
					};
					declarations
						.families
						.entry(key.to_string())
						.or_default()
						.entry(resource.value.clone())
						.or_default()
						.push(site);
					declarations
						.family_directories
						.insert(key.to_string(), family);
				}
				_ => {}
			}
		}
		for document in &index.documents {
			if document.parse_ok {
				continue;
			}
			let Some(family) = eu4().family_id_for(document.path.as_game_path()) else {
				continue;
			};
			declarations
				.broken_files
				.entry(family.to_string())
				.or_default()
				.push(DeclarationSite {
					mod_id: document.mod_id.clone(),
					path: document.path.clone(),
					line: 0,
					column: 0,
				});
		}
		declarations
	}

	fn push_event(&mut self, name: &str, site: DeclarationSite) {
		self.events.entry(name.to_string()).or_default().push(site);
	}

	/// The declarations of an event, by its full id or by its bare id.
	pub fn event_declarations(&self, name: &str) -> &[DeclarationSite] {
		self.events.get(name).map_or(&[], Vec::as_slice)
	}

	/// The declarations an event reference to `name` can resolve to.
	///
	/// The engine resolves a qualified id against the event that declares
	/// it, and a bare id against the tail of a qualified one. A qualified
	/// reference also resolves against the bare event of the same tail.
	///
	/// The declarations are borrowed and read as they are: a reference
	/// that resolves costs no allocation.
	pub fn event_declarations_for(
		&self,
		name: &str,
	) -> impl Iterator<Item = &DeclarationSite> + Clone {
		let qualified = self.event_declarations(name);
		let tail = name
			.rsplit_once('.')
			.map(|(_, tail)| self.event_declarations(tail))
			.unwrap_or_default();
		qualified.iter().chain(tail.iter())
	}

	/// Whether an event file declares the event under the id `name`.
	pub fn event_declared_under(&self, name: &str) -> bool {
		self.event_ids.contains_key(name)
	}

	/// The declarations of an event namespace.
	pub fn namespace_declarations(&self, name: &str) -> &[DeclarationSite] {
		self.namespaces.get(name).map_or(&[], Vec::as_slice)
	}

	/// The declarations of a content family's definition.
	pub fn family_declarations(&self, key: &str, name: &str) -> &[DeclarationSite] {
		self.families
			.get(key)
			.and_then(|declarations| declarations.get(name))
			.map_or(&[], Vec::as_slice)
	}

	/// The content family a family's declarations live in.
	pub fn family_directory(&self, key: &str) -> Option<&'static str> {
		self.family_directories.get(key).copied()
	}

	/// The sprite declarations of a sprite name.
	pub fn sprite_declarations(&self, name: &str) -> &[SpriteDeclaration] {
		self.sprites.get(name).map_or(&[], Vec::as_slice)
	}

	/// The files of a content family that did not parse.
	pub fn broken_files(&self, family: &str) -> &[DeclarationSite] {
		self.broken_files.get(family).map_or(&[], Vec::as_slice)
	}

	/// The key whose family declares the definitions a schema type names.
	///
	/// The schema names a type and the directory its definitions live in;
	/// the index names the key the family's declarations are recorded under.
	/// The two meet when the schema's directory is the content family's id.
	pub fn definition_key_for_type(&self, type_name: &str) -> Option<String> {
		let family = crate::game::eu4::cwt::schema_type_families().get(type_name)?;
		crate::graph::semantic::DEFINITION_REFERENCE_FAMILIES
			.iter()
			.find(|(_, directory)| *directory == family.as_str())
			.map(|(key, _)| (*key).to_string())
	}
}

/// The content families a sprite can be declared in.
const SPRITE_FAMILIES: &[&str] = &["interface", "common/interface", "gfx"];

/// The content family a definition key's declarations live in.
pub(crate) fn definition_reference_family(key: &str) -> Option<&'static str> {
	crate::graph::semantic::definition_reference_family(key)
}

/// What the analysis needs to explain a reference that does not resolve.
pub struct ReferenceContext<'a> {
	/// The declarations the index holds.
	pub declarations: &'a DeclarationIndex,
	/// The playset facts.
	pub facts: &'a PlaysetFacts,
	/// The files the effective playset contains.
	pub files: &'a HashSet<GamePathBuf>,
	/// Whether the base game is part of the input.
	pub base_loaded: bool,
	/// Whether the file inventory covers every file of the playset: no
	/// filter narrowed it.
	pub inventory_complete: bool,
	/// The base game's identity, as a label.
	pub base_identity: Option<String>,
	/// The analyzed input's identity, as a label.
	pub input_identity: String,
}

impl ReferenceContext<'_> {
	/// Why the reference to `name` does not resolve, or `None` when it does.
	pub fn diagnose(
		&self,
		kind: ReferenceKind,
		name: &str,
		referring_mod: &str,
	) -> Option<ReferenceDiagnosis> {
		match &kind {
			ReferenceKind::Event => self.diagnose_event(name, referring_mod),
			ReferenceKind::Namespace => {
				let providers = self.declarations.namespace_declarations(name);
				self.classify(
					&kind,
					name,
					providers.iter(),
					self.declarations.broken_files(EVENT_FAMILY).iter(),
					self.base_loaded && self.inventory_complete,
					referring_mod,
				)
			}
			ReferenceKind::Family(key) => {
				let providers = self.declarations.family_declarations(key, name);
				let family = self.declarations.family_directory(key);
				let broken = family
					.map(|family| self.declarations.broken_files(family))
					.unwrap_or_default();
				self.classify(
					&kind,
					name,
					providers.iter(),
					broken.iter(),
					self.base_loaded && self.inventory_complete,
					referring_mod,
				)
			}
			ReferenceKind::Sprite => self.diagnose_sprite(name, referring_mod),
			// The rules name a kind of definition foch records no
			// definitions of, so it cannot judge the reference.
			ReferenceKind::Type(_) => Some(ReferenceDiagnosis {
				kind: kind.clone(),
				name: name.to_string(),
				failure: ReferenceFailure::Unknown {
					reason: UnknownReason::NoRule,
				},
				providers: Vec::new(),
			}),
		}
	}

	/// Why an event reference does not resolve, or `None` when it does.
	///
	/// A qualified id names a namespace, and the namespace must be one the
	/// playset declares: a dot in an id is no evidence that it is. When the
	/// namespace is not declared, it is the root cause, so it is what the
	/// diagnostic reports.
	fn diagnose_event(&self, name: &str, referring_mod: &str) -> Option<ReferenceDiagnosis> {
		if let Some((namespace, _)) = name.rsplit_once('.')
			&& !self.declarations.event_declared_under(name)
			&& let Some(diagnosis) =
				self.diagnose(ReferenceKind::Namespace, namespace, referring_mod)
		{
			return Some(diagnosis);
		}
		self.classify(
			&ReferenceKind::Event,
			name,
			self.declarations.event_declarations_for(name),
			self.declarations.broken_files(EVENT_FAMILY).iter(),
			self.base_loaded && self.inventory_complete,
			referring_mod,
		)
	}

	/// Why a GUI's sprite does not load, or `None` when it does.
	fn diagnose_sprite(&self, name: &str, referring_mod: &str) -> Option<ReferenceDiagnosis> {
		let declarations = self.declarations.sprite_declarations(name);
		let effective = declarations
			.iter()
			.filter(|declaration| self.facts.is_effective(&declaration.site));
		let Some(declaration) = effective.clone().next_back() else {
			return self.classify(
				&ReferenceKind::Sprite,
				name,
				declarations.iter().map(|declaration| &declaration.site),
				SPRITE_FAMILIES
					.iter()
					.flat_map(|family| self.declarations.broken_files(family).iter()),
				self.base_loaded && self.inventory_complete,
				referring_mod,
			);
		};
		let Some(texture) = declaration.texture.as_ref() else {
			return Some(ReferenceDiagnosis {
				kind: ReferenceKind::Sprite,
				name: name.to_string(),
				failure: ReferenceFailure::UnloadableSprite,
				providers: sprite_sites(effective.clone()),
			});
		};
		if self.files.contains(texture) {
			return None;
		}
		if !self.inventory_complete {
			return Some(ReferenceDiagnosis {
				kind: ReferenceKind::Sprite,
				name: name.to_string(),
				failure: ReferenceFailure::Unknown {
					reason: UnknownReason::IncompleteScope,
				},
				providers: sprite_sites(effective.clone()),
			});
		}
		Some(ReferenceDiagnosis {
			kind: ReferenceKind::Sprite,
			name: name.to_string(),
			failure: ReferenceFailure::MissingTexture {
				texture: texture.clone(),
			},
			providers: sprite_sites(effective.clone()),
		})
	}

	/// Why a name that `providers` declares does not resolve.
	///
	/// The order of the answers matters: a definition the playset replaced
	/// away is sure, a file that failed to parse is a defect worth reading,
	/// a dependency the playset does not load is the likeliest home of the
	/// name, and only a scope that covers every file that could declare the
	/// name may call it missing.
	///
	/// The providers are borrowed, and the declaration sites are copied
	/// only into the diagnosis the analysis reports: a reference that
	/// resolves costs nothing.
	fn classify<'p, P, B>(
		&self,
		kind: &ReferenceKind,
		name: &str,
		providers: P,
		broken: B,
		scope_complete: bool,
		referring_mod: &str,
	) -> Option<ReferenceDiagnosis>
	where
		P: IntoIterator<Item = &'p DeclarationSite> + Clone,
		B: IntoIterator<Item = &'p DeclarationSite>,
	{
		// A declaration the playset loads decides the answer: one the
		// playset keeps resolves the reference, and one a later mod
		// replaces away is a sure failure.
		let mut loaded = false;
		for site in providers.clone() {
			if !self.facts.mod_is_loaded(&site.mod_id) {
				continue;
			}
			loaded = true;
			if self.facts.is_effective(site) {
				return None;
			}
		}
		if loaded {
			// The playset loads the mod that declares the name, but a later
			// mod replaces the directory the declaration lives in.
			let providers = owned_sites(providers);
			let shadowed = providers
				.iter()
				.filter(|site| self.facts.mod_is_loaded(&site.mod_id))
				.cloned()
				.collect::<Vec<_>>();
			return Some(ReferenceDiagnosis {
				kind: kind.clone(),
				name: name.to_string(),
				failure: ReferenceFailure::Replaced { shadowed },
				providers,
			});
		}
		if let Some(file) = broken.into_iter().next() {
			return Some(ReferenceDiagnosis {
				kind: kind.clone(),
				name: name.to_string(),
				failure: ReferenceFailure::BrokenSource { file: file.clone() },
				providers: owned_sites(providers),
			});
		}
		if let Some(dependency) = self.facts.missing_dependencies(referring_mod).first() {
			return Some(ReferenceDiagnosis {
				kind: kind.clone(),
				name: name.to_string(),
				failure: ReferenceFailure::MissingDependency {
					dependency: dependency.clone(),
				},
				providers: owned_sites(providers),
			});
		}
		if !scope_complete {
			return Some(ReferenceDiagnosis {
				kind: kind.clone(),
				name: name.to_string(),
				failure: ReferenceFailure::Unknown {
					reason: UnknownReason::IncompleteScope,
				},
				providers: owned_sites(providers),
			});
		}
		Some(ReferenceDiagnosis {
			kind: kind.clone(),
			name: name.to_string(),
			failure: ReferenceFailure::NotDeclared,
			providers: owned_sites(providers),
		})
	}
}

/// The declaration sites a provider list names, owned.
fn owned_sites<'p>(
	providers: impl IntoIterator<Item = &'p DeclarationSite>,
) -> Vec<DeclarationSite> {
	providers.into_iter().cloned().collect()
}

/// The declaration sites a sprite's declarations live at.
fn sprite_sites<'p>(
	declarations: impl Iterator<Item = &'p SpriteDeclaration>,
) -> Vec<DeclarationSite> {
	declarations
		.map(|declaration| declaration.site.clone())
		.collect()
}

/// The content family the event files live in.
const EVENT_FAMILY: &str = "events";

/// Whether the name is one the engine reads at load time, rather than one it
/// substitutes at run time.
fn is_static_name(value: &str) -> bool {
	!value.is_empty()
		&& !value.contains('$')
		&& !value.contains('[')
		&& !value.contains(']')
		&& !value.contains('@')
		&& !value.contains("event_target:")
		&& !value.contains("trigger_value:")
}

/// The references the index holds that do not resolve, with the reason.
///
/// Every reference is reported once: a reference whose namespace is not
/// declared is reported as such, because the namespace is the root cause.
pub fn unresolved_references(
	index: &SemanticIndex,
	context: &ReferenceContext<'_>,
) -> Vec<Finding> {
	let mut findings = Vec::new();
	let mut unknown_types: HashSet<(String, String)> = HashSet::new();
	for reference in &index.references {
		if reference.kind != SymbolKind::Event {
			continue;
		}
		// The engine resolves a template at run time.
		if reference.name.contains('$') || reference.name.contains('[') {
			continue;
		}
		if let Some(diagnosis) =
			context.diagnose(ReferenceKind::Event, &reference.name, &reference.mod_id)
		{
			findings.push(reference_finding(
				&reference.mod_id,
				&reference.path,
				reference.line,
				reference.column,
				&diagnosis,
				context,
			));
		}
	}
	for resource in &index.resource_references {
		if let Some(type_name) = resource.key.strip_prefix(TYPED_REFERENCE_KEY) {
			if !is_static_name(&resource.value) {
				continue;
			}
			let Some(key) = context.declarations.definition_key_for_type(type_name) else {
				// The schema types the value, but foch records no
				// definitions of that kind: it cannot judge the
				// reference. One report per mod and type keeps the lead
				// readable.
				if !crate::game::eu4::base::snapshot::is_base_game_mod_id(&resource.mod_id)
					&& unknown_types.insert((resource.mod_id.clone(), type_name.to_string()))
				{
					let diagnosis = ReferenceDiagnosis {
						kind: ReferenceKind::Type(type_name.to_string()),
						name: resource.value.clone(),
						failure: ReferenceFailure::Unknown {
							reason: UnknownReason::NoRule,
						},
						providers: Vec::new(),
					};
					findings.push(reference_finding(
						&resource.mod_id,
						&resource.path,
						resource.line,
						resource.column,
						&diagnosis,
						context,
					));
				}
				continue;
			};
			if let Some(diagnosis) = context.diagnose(
				ReferenceKind::Family(key),
				&resource.value,
				&resource.mod_id,
			) {
				findings.push(reference_finding(
					&resource.mod_id,
					&resource.path,
					resource.line,
					resource.column,
					&diagnosis,
					context,
				));
			}
			continue;
		}
		// GUI files spell a sprite reference `sprite`, `spriteType`,
		// `buttonSprite` or `quadTextureSprite`, and the extractor records
		// the spelling it found as the key.
		let is_sprite_reference = resource.key == SPRITE_REFERENCE_KEY
			|| resource.key == "spriteType"
			|| resource.key == "buttonSprite"
			|| resource.key == "quadTextureSprite";
		if !is_sprite_reference || !is_static_name(&resource.value) {
			continue;
		}
		if let Some(diagnosis) =
			context.diagnose(ReferenceKind::Sprite, &resource.value, &resource.mod_id)
		{
			findings.push(reference_finding(
				&resource.mod_id,
				&resource.path,
				resource.line,
				resource.column,
				&diagnosis,
				context,
			));
		}
	}
	findings
}

/// The finding a reference diagnosis is reported as.
fn reference_finding(
	mod_id: &str,
	path: &GamePathBuf,
	line: usize,
	column: usize,
	diagnosis: &ReferenceDiagnosis,
	context: &ReferenceContext<'_>,
) -> Finding {
	let (severity, channel) = failure_severity(&diagnosis.failure);
	Finding {
		rule_id: failure_rule_id(&diagnosis.failure).to_string(),
		severity,
		channel,
		message: failure_message(&diagnosis.kind, &diagnosis.name, &diagnosis.failure),
		mod_id: Some(mod_id.to_string()),
		path: Some(path.clone()),
		source_file: None,
		evidence: Some(failure_evidence(
			&diagnosis.kind,
			&diagnosis.name,
			diagnosis,
			context,
		)),
		line: Some(line),
		column: Some(column),
		confidence: Some(failure_confidence(&diagnosis.failure)),
	}
}

/// The message a failure is reported with.
fn failure_message(kind: &ReferenceKind, name: &str, failure: &ReferenceFailure) -> String {
	match failure {
		ReferenceFailure::NotDeclared => {
			format!(
				"no file of the playset declares the {} `{name}`",
				kind.as_str()
			)
		}
		ReferenceFailure::BrokenSource { file } => format!(
			"cannot tell whether the {} `{name}` exists: `{}` did not parse",
			kind.as_str(),
			file.path.as_str()
		),
		ReferenceFailure::Replaced { shadowed } => format!(
			"the {} `{name}` is declared, but the playset replaces `{}`",
			kind.as_str(),
			shadowed
				.first()
				.map(|site| site.path.as_str())
				.unwrap_or_default()
		),
		ReferenceFailure::MissingDependency { dependency } => format!(
			"the {} `{name}` is not declared; `{dependency}` is a dependency of the referring mod that the playset does not load",
			kind.as_str()
		),
		ReferenceFailure::MissingTexture { texture } => format!(
			"the sprite `{name}` names the texture `{}`, which the playset does not contain",
			texture.as_str()
		),
		ReferenceFailure::UnloadableSprite => {
			format!("the sprite `{name}` is declared but names no texture")
		}
		ReferenceFailure::Unknown { reason } => format!(
			"cannot tell whether the {} `{name}` exists: {}",
			kind.as_str(),
			reason.as_str()
		),
	}
}

/// The evidence a finding carries: the reference, the reason, the
/// declarations the analysis found, and the identities it read them from.
fn failure_evidence(
	kind: &ReferenceKind,
	name: &str,
	diagnosis: &ReferenceDiagnosis,
	context: &ReferenceContext<'_>,
) -> String {
	let providers = diagnosis
		.providers
		.iter()
		.map(|site| {
			format!(
				"{}:{}:{}:{}",
				site.mod_id,
				site.path.as_str(),
				site.line,
				site.column
			)
		})
		.collect::<Vec<_>>()
		.join(", ");
	format!(
		"reference={} name={} reason={} {}providers=[{}] base={} input={}",
		kind.as_str(),
		name,
		failure_reason(&diagnosis.failure),
		failure_detail(&diagnosis.failure),
		providers,
		context.base_identity.as_deref().unwrap_or("absent"),
		context.input_identity,
	)
}

/// The reason's name, as the evidence reads it.
fn failure_reason(failure: &ReferenceFailure) -> &'static str {
	match failure {
		ReferenceFailure::NotDeclared => "not_declared",
		ReferenceFailure::BrokenSource { .. } => "broken_source",
		ReferenceFailure::Replaced { .. } => "replaced",
		ReferenceFailure::MissingDependency { .. } => "missing_dependency",
		ReferenceFailure::MissingTexture { .. } => "missing_texture",
		ReferenceFailure::UnloadableSprite => "unloadable_sprite",
		ReferenceFailure::Unknown { reason } => reason.as_str(),
	}
}

/// The detail a failure carries, as the evidence reads it.
fn failure_detail(failure: &ReferenceFailure) -> String {
	match failure {
		ReferenceFailure::MissingDependency { dependency } => {
			format!("dependency={dependency} ")
		}
		ReferenceFailure::BrokenSource { file } => {
			format!("broken={}:{} ", file.mod_id, file.path.as_str())
		}
		ReferenceFailure::MissingTexture { texture } => {
			format!("texture={} ", texture.as_str())
		}
		_ => String::new(),
	}
}

/// How sure the analysis is of a failure.
fn failure_confidence(failure: &ReferenceFailure) -> f32 {
	match failure {
		ReferenceFailure::NotDeclared | ReferenceFailure::Replaced { .. } => 0.9,
		ReferenceFailure::MissingTexture { .. } => 0.85,
		ReferenceFailure::BrokenSource { .. } => 0.8,
		ReferenceFailure::MissingDependency { .. } => 0.6,
		ReferenceFailure::UnloadableSprite => 0.5,
		ReferenceFailure::Unknown { .. } => 0.4,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::{
		DocumentFamily, DocumentRecord, ModCandidate, ResourceReference, ScopeSet,
		SymbolDefinition, SymbolReference,
	};
	use crate::playset::PlaysetEntry;
	use crate::playset::descriptor::ModDescriptor;

	/// The mod id the base game declares under.
	const BASE_MOD_ID: &str = "__game__eu4";

	fn game_path(text: &str) -> GamePathBuf {
		GamePathBuf::parse(text).expect("a valid game path")
	}

	fn candidate_with(
		mod_id: &str,
		enabled: bool,
		dependencies: &[&str],
		replace_paths: &[&str],
	) -> ModCandidate {
		candidate_named(mod_id, mod_id, enabled, dependencies, replace_paths)
	}

	/// A mod of the playset under a display name of its own, as a workshop
	/// mod has: the launcher knows it by that name, not by its id.
	fn candidate_named(
		mod_id: &str,
		name: &str,
		enabled: bool,
		dependencies: &[&str],
		replace_paths: &[&str],
	) -> ModCandidate {
		ModCandidate {
			entry: PlaysetEntry {
				id: Some(mod_id.to_string()),
				display_name: Some(name.to_string()),
				enabled,
				position: None,
				steam_id: None,
				workshop_identity: None,
				root_path: None,
			},
			mod_id: mod_id.to_string(),
			root_path: None,
			descriptor_path: None,
			descriptor: Some(ModDescriptor {
				name: name.to_string(),
				dependencies: dependencies
					.iter()
					.map(|item| (*item).to_string())
					.collect(),
				replace_path: replace_paths.iter().map(|item| game_path(item)).collect(),
				..ModDescriptor::default()
			}),
			workshop_identity: None,
			descriptor_error: None,
			files: Vec::new(),
		}
	}

	/// A playset the analysis reads.
	struct Fixture {
		index: SemanticIndex,
		mods: Vec<ModCandidate>,
		files: HashSet<GamePathBuf>,
		base_loaded: bool,
		inventory_complete: bool,
	}

	impl Fixture {
		fn new() -> Self {
			Self {
				index: SemanticIndex::default(),
				mods: Vec::new(),
				files: HashSet::new(),
				base_loaded: true,
				inventory_complete: true,
			}
		}

		fn mod_with(mut self, mod_id: &str, dependencies: &[&str], replace_paths: &[&str]) -> Self {
			self.mods
				.push(candidate_with(mod_id, true, dependencies, replace_paths));
			self
		}

		/// A mod the playset contains but does not load, under a display
		/// name of its own.
		fn disabled_mod(&mut self, mod_id: &str, name: &str) {
			self.mods
				.push(candidate_named(mod_id, name, false, &[], &[]));
		}

		fn declare_building(&mut self, name: &str, mod_id: &str, file: &str) {
			self.index.resource_references.push(ResourceReference {
				key: "building_definition".to_string(),
				value: name.to_string(),
				mod_id: mod_id.to_string(),
				path: game_path(file),
				line: 1,
				column: 1,
			});
		}

		fn reference_building(&mut self, name: &str, mod_id: &str, file: &str, line: usize) {
			self.index.resource_references.push(ResourceReference {
				key: "type_reference:building".to_string(),
				value: name.to_string(),
				mod_id: mod_id.to_string(),
				path: game_path(file),
				line,
				column: 3,
			});
		}

		fn declare_namespace(&mut self, name: &str, mod_id: &str, file: &str) {
			self.index.resource_references.push(ResourceReference {
				key: EVENT_NAMESPACE_KEY.to_string(),
				value: name.to_string(),
				mod_id: mod_id.to_string(),
				path: game_path(file),
				line: 2,
				column: 1,
			});
		}

		fn declare_event(&mut self, id: &str, mod_id: &str, file: &str) {
			self.index.definitions.push(SymbolDefinition {
				kind: SymbolKind::Event,
				name: id.to_string(),
				module: "events".to_string(),
				local_name: "country_event".to_string(),
				mod_id: mod_id.to_string(),
				path: game_path(file),
				line: 3,
				column: 1,
				scope_id: 0,
				declared_this_type: crate::model::MaybeScope::Unknown,
				inferred_this_type: crate::model::MaybeScope::Unknown,
				inferred_this_mask: ScopeSet::default(),
				inferred_from_mask: ScopeSet::default(),
				inferred_root_mask: ScopeSet::default(),
				required_params: Vec::new(),
				optional_params: Vec::new(),
				scope_param_names: Vec::new(),
			});
		}

		fn reference_event(&mut self, id: &str, mod_id: &str, file: &str, line: usize) {
			self.index.references.push(SymbolReference {
				kind: SymbolKind::Event,
				name: id.to_string(),
				module: "events".to_string(),
				mod_id: mod_id.to_string(),
				path: game_path(file),
				line,
				column: 5,
				scope_id: 0,
				provided_params: Vec::new(),
				param_bindings: Vec::new(),
				schema_alias: false,
			});
		}

		fn declare_sprite(&mut self, name: &str, texture: &str, mod_id: &str, file: &str) {
			self.index.resource_references.push(ResourceReference {
				key: format!("{SPRITE_DEFINITION_KEY}{name}"),
				value: texture.to_string(),
				mod_id: mod_id.to_string(),
				path: game_path(file),
				line: 6,
				column: 1,
			});
		}

		fn reference_sprite(&mut self, name: &str, mod_id: &str, file: &str, line: usize) {
			self.index.resource_references.push(ResourceReference {
				key: SPRITE_REFERENCE_KEY.to_string(),
				value: name.to_string(),
				mod_id: mod_id.to_string(),
				path: game_path(file),
				line,
				column: 7,
			});
		}

		fn broken_file(&mut self, mod_id: &str, file: &str) {
			self.index.documents.push(DocumentRecord {
				mod_id: mod_id.to_string(),
				path: game_path(file),
				family: DocumentFamily::Clausewitz,
				parse_ok: false,
			});
		}

		fn findings(&self) -> Vec<Finding> {
			let declarations = DeclarationIndex::from_index(&self.index);
			let facts = PlaysetFacts::from_mods(&self.mods);
			let context = ReferenceContext {
				declarations: &declarations,
				facts: &facts,
				files: &self.files,
				base_loaded: self.base_loaded,
				inventory_complete: self.inventory_complete,
				base_identity: Some("eu4@1.37".to_string()),
				input_identity: "dlc_load.json".to_string(),
			};
			unresolved_references(&self.index, &context)
		}
	}

	#[test]
	fn the_schema_declares_the_building_type_in_the_buildings_family() {
		assert_eq!(
			crate::game::eu4::cwt::schema_type_families()
				.get("building")
				.map(String::as_str),
			Some("common/buildings")
		);
	}

	#[test]
	fn a_name_nothing_declares_is_reported_as_not_declared() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.declare_building(
			"other_building",
			BASE_MOD_ID,
			"common/buildings/00_buildings.txt",
		);
		fixture.reference_building(
			"my_building",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-not-declared");
		assert_eq!(findings[0].severity, Severity::Error);
		assert_eq!(findings[0].channel, FindingChannel::Strict);
		assert_eq!(findings[0].mod_id.as_deref(), Some("mod-a"));
		assert_eq!(
			findings[0].path.as_ref().map(|path| path.as_str()),
			Some("common/buildings/01_buildings.txt")
		);
		assert_eq!(findings[0].line, Some(7));
	}

	#[test]
	fn a_building_the_base_declares_resolves() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.declare_building(
			"my_building",
			BASE_MOD_ID,
			"common/buildings/00_buildings.txt",
		);
		fixture.reference_building(
			"my_building",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		assert!(fixture.findings().is_empty());
	}

	#[test]
	fn a_legal_override_passes() {
		let mut fixture = Fixture::new()
			.mod_with("mod-a", &[], &[])
			.mod_with("mod-b", &[], &[]);
		fixture.declare_building("my_building", "mod-a", "common/buildings/a.txt");
		fixture.declare_building("my_building", "mod-b", "common/buildings/b.txt");
		fixture.reference_building("my_building", "mod-a", "common/buildings/a.txt", 7);
		assert!(fixture.findings().is_empty());
	}

	#[test]
	fn a_definition_the_playset_replaces_is_reported_as_replaced() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]).mod_with(
			"mod-b",
			&[],
			&["common/buildings"],
		);
		fixture.declare_building("my_building", "mod-a", "common/buildings/a.txt");
		fixture.reference_building("my_building", "mod-a", "common/buildings/a.txt", 7);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-replaced");
		assert_eq!(findings[0].severity, Severity::Error);
		let evidence = findings[0].evidence.as_deref().unwrap_or_default();
		assert!(
			evidence.contains("mod-a:common/buildings/a.txt:1:1"),
			"the evidence names the declaration the playset dropped: {evidence}"
		);
	}

	#[test]
	fn a_family_file_that_did_not_parse_leaves_the_name_unknown() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.broken_file("mod-b", "common/buildings/00_buildings.txt");
		fixture.declare_building(
			"other_building",
			BASE_MOD_ID,
			"common/buildings/00_buildings.txt",
		);
		fixture.reference_building(
			"my_building",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-broken-source");
		assert_eq!(findings[0].channel, FindingChannel::Advisory);
	}

	#[test]
	fn a_dependency_the_playset_does_not_load_is_the_likely_home_of_the_name() {
		let mut fixture = Fixture::new().mod_with("mod-a", &["Needed Mod"], &[]);
		fixture.declare_building(
			"other_building",
			BASE_MOD_ID,
			"common/buildings/00_buildings.txt",
		);
		fixture.reference_building(
			"my_building",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(
			findings[0].rule_id,
			"unresolved-reference-missing-dependency"
		);
		assert_eq!(findings[0].channel, FindingChannel::Advisory);
		assert!(
			findings[0]
				.evidence
				.as_deref()
				.unwrap_or_default()
				.contains("Needed Mod")
		);
	}

	#[test]
	fn a_dependency_on_a_mod_the_playset_disables_is_the_likely_home_of_the_name() {
		let mut fixture = Fixture::new().mod_with("mod-a", &["Needed Mod"], &[]);
		fixture.disabled_mod("mod-dep", "Needed Mod");
		fixture.declare_building("my_building", "mod-dep", "common/buildings/dep.txt");
		fixture.reference_building(
			"my_building",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(
			findings[0].rule_id,
			"unresolved-reference-missing-dependency"
		);
		let evidence = findings[0].evidence.as_deref().unwrap_or_default();
		assert!(evidence.contains("dependency=Needed Mod"), "{evidence}");
		assert!(
			evidence.contains("providers=[mod-dep:common/buildings/dep.txt:1:1]"),
			"{evidence}"
		);
	}

	#[test]
	fn a_declaration_the_playset_does_not_load_is_named_as_a_provider() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.disabled_mod("mod-dep", "Needed Mod");
		fixture.declare_building("my_building", "mod-dep", "common/buildings/dep.txt");
		fixture.reference_building(
			"my_building",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-not-declared");
		let evidence = findings[0].evidence.as_deref().unwrap_or_default();
		assert!(
			evidence.contains("providers=[mod-dep:common/buildings/dep.txt:1:1]"),
			"{evidence}"
		);
	}

	#[test]
	fn a_broken_file_still_names_the_declarations_the_analysis_found() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.disabled_mod("mod-dep", "Needed Mod");
		fixture.broken_file("mod-b", "common/buildings/00_buildings.txt");
		fixture.declare_building("my_building", "mod-dep", "common/buildings/dep.txt");
		fixture.reference_building(
			"my_building",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-broken-source");
		let evidence = findings[0].evidence.as_deref().unwrap_or_default();
		assert!(
			evidence.contains("providers=[mod-dep:common/buildings/dep.txt:1:1]"),
			"{evidence}"
		);
	}

	#[test]
	fn a_name_the_analysis_cannot_judge_still_names_the_declarations_it_found() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.base_loaded = false;
		fixture.disabled_mod("mod-dep", "Needed Mod");
		fixture.declare_building("my_building", "mod-dep", "common/buildings/dep.txt");
		fixture.reference_building(
			"my_building",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-unknown");
		let evidence = findings[0].evidence.as_deref().unwrap_or_default();
		assert!(
			evidence.contains("providers=[mod-dep:common/buildings/dep.txt:1:1]"),
			"{evidence}"
		);
	}

	#[test]
	fn only_a_mod_that_loads_later_replaces_a_declaration_away() {
		let mut fixture = Fixture::new()
			.mod_with("mod-a", &[], &["common/buildings"])
			.mod_with("mod-b", &[], &[]);
		fixture.declare_building("my_building", "mod-b", "common/buildings/b.txt");
		fixture.reference_building("my_building", "mod-a", "common/buildings/a.txt", 7);
		assert!(fixture.findings().is_empty());
	}

	#[test]
	fn a_mod_that_replaces_another_directory_leaves_the_declaration_alone() {
		let mut fixture = Fixture::new()
			.mod_with("mod-a", &[], &[])
			.mod_with("mod-b", &[], &["common/ideas"])
			.mod_with("mod-c", &[], &["common/buildings"]);
		fixture.declare_building("my_building", "mod-a", "common/buildings/a.txt");
		fixture.reference_building("my_building", "mod-a", "common/buildings/a.txt", 7);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-replaced");
	}

	#[test]
	fn a_mod_that_replaces_a_parent_directory_replaces_the_declaration_away() {
		let mut fixture =
			Fixture::new()
				.mod_with("mod-a", &[], &[])
				.mod_with("mod-b", &[], &["common"]);
		fixture.declare_building("my_building", "mod-a", "common/buildings/a.txt");
		fixture.reference_building("my_building", "mod-a", "common/buildings/a.txt", 7);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-replaced");
	}

	#[test]
	fn a_dependency_naming_a_disabled_mod_by_its_id_is_missing() {
		let mut fixture = Fixture::new().mod_with("mod-a", &["mod-dep"], &[]);
		fixture.disabled_mod("mod-dep", "Needed Mod");
		fixture.reference_building(
			"my_building",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(
			findings[0].rule_id,
			"unresolved-reference-missing-dependency"
		);
		let evidence = findings[0].evidence.as_deref().unwrap_or_default();
		assert!(evidence.contains("dependency=mod-dep"), "{evidence}");
	}

	#[test]
	fn a_dependency_the_playset_loads_is_not_missing() {
		let mut fixture = Fixture::new()
			.mod_with("mod-a", &["mod-dep"], &[])
			.mod_with("mod-dep", &[], &[]);
		fixture.reference_building(
			"my_building",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-not-declared");
	}

	#[test]
	fn a_name_is_not_called_missing_when_the_base_game_is_not_part_of_the_input() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.base_loaded = false;
		fixture.declare_building(
			"other_building",
			"mod-a",
			"common/buildings/00_buildings.txt",
		);
		fixture.reference_building(
			"my_building",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-unknown");
		assert_eq!(findings[0].severity, Severity::Info);
	}

	#[test]
	fn a_dynamic_name_is_not_a_reference() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.reference_building(
			"$building$",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		fixture.reference_event("$EVENT$", "mod-a", "events/flavor.txt", 7);
		assert!(fixture.findings().is_empty());
	}

	#[test]
	fn a_kind_of_definition_the_rules_do_not_describe_is_reported_once() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		for (line, name) in [(4, "my_policy"), (5, "other_policy")] {
			fixture.index.resource_references.push(ResourceReference {
				key: "type_reference:policy".to_string(),
				value: name.to_string(),
				mod_id: "mod-a".to_string(),
				path: game_path("common/scripted_effects/a.txt"),
				line,
				column: 1,
			});
		}
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-unknown");
		assert_eq!(findings[0].severity, Severity::Info);
		assert!(
			findings[0]
				.evidence
				.as_deref()
				.unwrap_or_default()
				.contains("reason=no_rule")
		);
	}

	#[test]
	fn the_diagnostic_names_the_mod_the_file_the_position_and_the_identities() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.declare_building(
			"other_building",
			BASE_MOD_ID,
			"common/buildings/00_buildings.txt",
		);
		fixture.reference_building(
			"my_building",
			"mod-a",
			"common/buildings/01_buildings.txt",
			7,
		);
		let findings = fixture.findings();
		let evidence = findings[0].evidence.as_deref().unwrap_or_default();
		assert!(
			evidence.contains("reference=building_definition"),
			"{evidence}"
		);
		assert!(evidence.contains("name=my_building"), "{evidence}");
		assert!(evidence.contains("reason=not_declared"), "{evidence}");
		assert!(evidence.contains("base=eu4@1.37"), "{evidence}");
		assert!(evidence.contains("input=dlc_load.json"), "{evidence}");
	}

	#[test]
	fn an_event_reference_whose_namespace_is_not_declared_is_reported() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.reference_event("flavor_fra.9100", "mod-a", "events/flavor.txt", 4);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-not-declared");
		assert!(
			findings[0].message.contains("event_namespace"),
			"{}",
			findings[0].message
		);
	}

	#[test]
	fn an_event_namespace_declared_in_another_file_resolves_the_reference() {
		let mut fixture = Fixture::new()
			.mod_with("mod-a", &[], &[])
			.mod_with("mod-b", &[], &[]);
		fixture.declare_namespace("flavor_fra", "mod-b", "events/other.txt");
		fixture.declare_event("flavor_fra.9100", "mod-b", "events/other.txt");
		fixture.reference_event("flavor_fra.9100", "mod-a", "events/flavor.txt", 4);
		assert!(fixture.findings().is_empty());
	}

	#[test]
	fn an_event_declared_under_its_qualified_id_needs_no_namespace() {
		let mut fixture = Fixture::new()
			.mod_with("mod-a", &[], &[])
			.mod_with("mod-b", &[], &[]);
		fixture.declare_event("flavor_fra.9100", "mod-b", "events/other.txt");
		fixture.reference_event("flavor_fra.9100", "mod-a", "events/flavor.txt", 4);
		assert!(fixture.findings().is_empty());
	}

	#[test]
	fn an_event_reference_that_resolves_by_tail_still_needs_its_namespace() {
		let mut fixture = Fixture::new()
			.mod_with("mod-a", &[], &[])
			.mod_with("mod-b", &[], &[]);
		fixture.declare_event("9100", "mod-b", "events/other.txt");
		fixture.reference_event("flavor_fra.9100", "mod-a", "events/flavor.txt", 4);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert!(
			findings[0].message.contains("event_namespace"),
			"{}",
			findings[0].message
		);
	}

	#[test]
	fn a_broken_events_file_leaves_the_namespace_unknown() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.broken_file("mod-b", "events/flavor.txt");
		fixture.reference_event("flavor_fra.9100", "mod-a", "events/flavor.txt", 4);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-broken-source");
	}

	#[test]
	fn a_sprite_nothing_declares_is_reported_as_missing() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.reference_sprite("GFX_missing", "mod-a", "interface/main.gui", 9);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-not-declared");
		assert!(
			findings[0].message.contains("sprite"),
			"{}",
			findings[0].message
		);
	}

	#[test]
	fn a_sprite_whose_texture_is_missing_is_reported_as_missing_its_texture() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.declare_sprite(
			"GFX_foo",
			"gfx/interface/foo.dds",
			"mod-a",
			"interface/main.gfx",
		);
		fixture.reference_sprite("GFX_foo", "mod-a", "interface/main.gui", 9);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-missing-texture");
		assert_eq!(findings[0].severity, Severity::Error);
	}

	#[test]
	fn a_sprite_that_names_no_texture_is_reported_as_unloadable() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.declare_sprite("GFX_foo", "", "mod-a", "interface/main.gfx");
		fixture.reference_sprite("GFX_foo", "mod-a", "interface/main.gui", 9);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(
			findings[0].rule_id,
			"unresolved-reference-unloadable-sprite"
		);
		assert_eq!(findings[0].channel, FindingChannel::Advisory);
	}

	#[test]
	fn a_texture_the_playset_does_not_hold_is_not_called_missing_when_the_inventory_is_narrowed() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.inventory_complete = false;
		fixture.declare_sprite(
			"GFX_foo",
			"gfx/interface/foo.dds",
			"mod-a",
			"interface/main.gfx",
		);
		fixture.reference_sprite("GFX_foo", "mod-a", "interface/main.gui", 9);
		let findings = fixture.findings();
		assert_eq!(findings.len(), 1);
		assert_eq!(findings[0].rule_id, "unresolved-reference-unknown");
	}

	#[test]
	fn a_sprite_the_playset_holds_loads() {
		let mut fixture = Fixture::new().mod_with("mod-a", &[], &[]);
		fixture.declare_sprite(
			"GFX_foo",
			"gfx/interface/foo.dds",
			"mod-a",
			"interface/main.gfx",
		);
		fixture.files.insert(game_path("gfx/interface/foo.dds"));
		fixture.reference_sprite("GFX_foo", "mod-a", "interface/main.gui", 9);
		assert!(fixture.findings().is_empty());
	}
}
