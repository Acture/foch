//! Bundle-independent runtime layers shared by player and test launches.

use crate::{layout, platform};
use sha2::{Digest, Sha256};
use std::{
	fs, io,
	path::{Path, PathBuf},
};

#[derive(Clone, Debug, serde::Serialize)]
pub struct Excluded {
	pub path: String,
	pub sha256: Option<String>,
}

pub struct RuntimeLayer {
	pub directory: PathBuf,
	pub excluded: Vec<Excluded>,
	pub fonts_sha256: String,
	game_root: PathBuf,
	longest_relative: usize,
	remove_on_drop: bool,
}

pub use foch::plugin::deployment::ensure_outside_game;

impl RuntimeLayer {
	/// Create a fresh owned layer. Existing directories and locations inside
	/// the source installation are refused, never adopted or overwritten.
	pub fn prepare(game_root: &Path, runtime_base: &Path, run_id: &str) -> io::Result<Self> {
		if run_id.is_empty()
			|| !run_id
				.bytes()
				.all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
		{
			return Err(io::Error::other("invalid runtime run id"));
		}
		let game_root = game_root.canonicalize()?;
		ensure_outside_game(&game_root, runtime_base)?;
		fs::create_dir_all(runtime_base)?;
		let base = runtime_base.canonicalize()?;
		let directory = PathBuf::from(foch::plugin::deployment::path_text(&base)?).join(run_id);
		ensure_outside_game(&game_root, &base)?;
		fs::create_dir(&directory)?;
		let mut layer = Self {
			directory,
			excluded: Vec::new(),
			fonts_sha256: String::new(),
			game_root: game_root.clone(),
			longest_relative: 0,
			remove_on_drop: true,
		};
		fs::write(layer.directory.join("foch-runtime"), "1\n")?;
		for entry in fs::read_dir(&game_root)? {
			let entry = entry?;
			let name = entry.file_name();
			let name = name.to_string_lossy();
			let is_dir = entry.file_type()?.is_dir();
			if name.eq_ignore_ascii_case("gfx") && is_dir {
				layer.longest_relative = layer
					.longest_relative
					.max(deepest_relative(&entry.path(), name.len()));
				layer.prepare_gfx(&entry.path())?;
				continue;
			}
			match layout::classify_entry(&name, is_dir) {
				layout::LayerAction::Copy => {
					fs::copy(entry.path(), layer.directory.join(&*name))?;
				}
				layout::LayerAction::Link => {
					layer.longest_relative = layer
						.longest_relative
						.max(deepest_relative(&entry.path(), name.len()));
					platform::link_dir(&entry.path(), &layer.directory.join(&*name))?;
				}
				layout::LayerAction::Skip => layer.excluded.push(Excluded {
					path: name.into_owned(),
					sha256: if is_dir {
						None
					} else {
						Some(format!("{:x}", Sha256::digest(fs::read(entry.path())?)))
					},
				}),
			}
		}
		fs::create_dir_all(layer.directory.join("gfx/fonts/eu4-unicode"))?;
		fs::write(layer.directory.join("userdir.txt"), "")?;
		fs::write(layer.directory.join("steam_appid.txt"), "236850")?;
		if let Some(error) = layer.budget_error() {
			return Err(io::Error::other(error));
		}
		Ok(layer)
	}

	fn prepare_gfx(&mut self, source: &Path) -> io::Result<()> {
		let destination = self.directory.join("gfx");
		fs::create_dir(&destination)?;
		for entry in fs::read_dir(source)? {
			let entry = entry?;
			let name = entry.file_name();
			if name.to_string_lossy().eq_ignore_ascii_case("fonts") {
				let fonts = destination.join("fonts");
				fs::create_dir(&fonts)?;
				let mut files = fs::read_dir(entry.path())?.collect::<io::Result<Vec<_>>>()?;
				files.sort_by_key(|item| item.file_name());
				let mut digest = Sha256::new();
				for font in files {
					if font
						.file_name()
						.to_string_lossy()
						.eq_ignore_ascii_case("eu4-unicode")
					{
						continue;
					}
					if font.file_type()?.is_dir() {
						platform::link_dir(&font.path(), &fonts.join(font.file_name()))?;
					} else {
						let bytes = fs::read(font.path())?;
						digest.update(font.file_name().to_string_lossy().as_bytes());
						digest.update(Sha256::digest(&bytes));
						fs::write(fonts.join(font.file_name()), bytes)?;
					}
				}
				self.fonts_sha256 = format!("{:x}", digest.finalize());
			} else if entry.file_type()?.is_dir() {
				platform::link_dir(&entry.path(), &destination.join(name))?;
			} else {
				fs::copy(entry.path(), destination.join(name))?;
			}
		}
		Ok(())
	}

	pub fn redirect_unicode_cache(&self, cache: &Path) -> io::Result<()> {
		ensure_outside_game(&self.game_root, cache)?;
		fs::create_dir_all(cache)?;
		let directory = self.directory.join("gfx/fonts/eu4-unicode");
		fs::remove_dir(&directory)?;
		platform::link_dir(&cache.canonicalize()?, &directory)
	}

	pub fn budget_error(&self) -> Option<String> {
		(!layout::fits_path_budget(self.directory.to_string_lossy().encode_utf16().count(), self.longest_relative)).then(|| format!(
			"runtime layer path {} is too long for the game's files (deepest relative {} chars); choose a shorter runtime base",
			self.directory.display(), self.longest_relative
		))
	}

	/// Player games keep running when the launching manager exits. Their layer
	/// is retained with an ownership marker, and never swept as a test session.
	pub fn retain(&mut self) -> io::Result<PathBuf> {
		fs::write(self.directory.join("foch-player-runtime"), "1\n")?;
		self.remove_on_drop = false;
		Ok(self.directory.clone())
	}
}

/// Start an ordinary player game; dropping its Child handle does not stop it.
pub fn spawn_player(
	directory: &Path,
	user_dir: &Path,
	args: &[std::ffi::OsString],
) -> io::Result<std::process::Child> {
	std::process::Command::new(directory.join("eu4.exe"))
		.current_dir(directory)
		.arg(format!("-userdir={}", user_dir.display()))
		.args(args)
		.env("SteamAppId", "236850")
		.env("SteamGameId", "236850")
		.stdin(std::process::Stdio::null())
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::null())
		.spawn()
}

impl Drop for RuntimeLayer {
	fn drop(&mut self) {
		if self.remove_on_drop {
			platform::remove_layer(&self.directory);
		}
	}
}

fn deepest_relative(root: &Path, prefix: usize) -> usize {
	let mut deepest = prefix;
	if let Ok(entries) = fs::read_dir(root) {
		for entry in entries.flatten() {
			let child = prefix + 1 + entry.file_name().to_string_lossy().encode_utf16().count();
			deepest = deepest.max(if entry.path().is_dir() {
				deepest_relative(&entry.path(), child)
			} else {
				child
			});
		}
	}
	deepest
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn layer_excludes_old_loaders_owns_font_writes_and_cleans_nested_links() {
		let source = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		fs::create_dir_all(source.path().join("gfx/fonts/eu4-unicode/cache")).unwrap();
		fs::create_dir_all(source.path().join("gfx/interface")).unwrap();
		fs::create_dir_all(source.path().join("plugins")).unwrap();
		fs::write(source.path().join("eu4.exe"), "exe").unwrap();
		fs::write(source.path().join("VERSION.dll"), "old loader").unwrap();
		fs::write(source.path().join("gfx/fonts/base.fnt"), "original font").unwrap();
		fs::write(source.path().join("gfx/interface/file"), "linked resource").unwrap();
		fs::write(source.path().join("plugins/plugin64.dll"), "old patch").unwrap();
		let layer = RuntimeLayer::prepare(source.path(), output.path(), "fixture").unwrap();
		assert!(!layer.directory.join("VERSION.dll").exists());
		assert!(!layer.directory.join("plugins").exists());
		assert_eq!(
			layer
				.excluded
				.iter()
				.find(|item| item.path == "VERSION.dll")
				.unwrap()
				.sha256
				.as_ref()
				.unwrap()
				.len(),
			64
		);
		fs::write(layer.directory.join("gfx/fonts/base.fnt"), "new font").unwrap();
		fs::write(
			layer.directory.join("gfx/fonts/eu4-unicode/cache.txt"),
			"new cache",
		)
		.unwrap();
		assert_eq!(
			fs::read_to_string(source.path().join("gfx/fonts/base.fnt")).unwrap(),
			"original font"
		);
		drop(layer);
		assert!(!output.path().join("fixture").exists());
		assert_eq!(
			fs::read_to_string(source.path().join("gfx/interface/file")).unwrap(),
			"linked resource"
		);
		assert!(RuntimeLayer::prepare(source.path(), source.path(), "unsafe").is_err());
	}

	#[test]
	fn retained_player_layers_survive_drop_and_test_sweeping() {
		let source = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		let mut layer = RuntimeLayer::prepare(source.path(), output.path(), "player").unwrap();
		let directory = layer.retain().unwrap();
		drop(layer);
		crate::sweep_runtime_base(output.path(), std::time::Duration::ZERO);
		assert!(directory.exists());
		platform::remove_layer(&directory);
		assert!(!directory.exists());
		assert!(
			RuntimeLayer::prepare(source.path(), &source.path().join("new-base"), "unsafe")
				.is_err()
		);
		assert!(!source.path().join("new-base").exists());
	}

	#[test]
	fn unicode_cache_redirect_handles_mixed_path_separators_without_touching_source() {
		let source = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		let cache = tempfile::tempdir().unwrap();
		let layer = RuntimeLayer::prepare(source.path(), output.path(), "cache").unwrap();
		layer.redirect_unicode_cache(cache.path()).unwrap();
		fs::write(
			layer.directory.join("gfx/fonts/eu4-unicode/generated"),
			"cache",
		)
		.unwrap();
		drop(layer);
		assert_eq!(
			fs::read_to_string(cache.path().join("generated")).unwrap(),
			"cache"
		);
		assert!(!source.path().join("gfx").exists());
	}

	#[test]
	fn links_handle_shell_metacharacters_as_literal_path_names() {
		let root = tempfile::tempdir().unwrap();
		let source = root.path().join("source&%FOCH_TEST_PATH%!");
		let output = root.path().join("output&%FOCH_TEST_PATH%!");
		fs::create_dir_all(source.join("gfx/interface")).unwrap();
		fs::write(source.join("gfx/interface/file"), "original").unwrap();
		let layer = RuntimeLayer::prepare(&source, &output, "literal").unwrap();
		assert_eq!(
			fs::read_to_string(layer.directory.join("gfx/interface/file")).unwrap(),
			"original"
		);
		drop(layer);
		assert!(!output.join("literal").exists());
		assert_eq!(
			fs::read_to_string(source.join("gfx/interface/file")).unwrap(),
			"original"
		);
	}

	#[test]
	fn writable_roots_cannot_reach_the_game_through_a_link() {
		let source = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		let link = output.path().join("linked-game");
		platform::link_dir(source.path(), &link).unwrap();
		assert!(ensure_outside_game(source.path(), &link.join("new-state")).is_err());
		assert!(!source.path().join("new-state").exists());
		platform::remove_layer(&link);
		assert!(source.path().exists());
		let sibling_source = output.path().join("game");
		fs::create_dir(&sibling_source).unwrap();
		assert!(
			ensure_outside_game(
				&sibling_source,
				&output.path().join("missing/../game/profile")
			)
			.is_err()
		);
		assert!(!output.path().join("missing").exists());
		assert!(!sibling_source.join("profile").exists());
		// The data root can be outside while a nested cache points at the game.
		let cache_link = output.path().join("cache");
		platform::link_dir(source.path(), &cache_link).unwrap();
		let layer = RuntimeLayer::prepare(source.path(), &output.path().join("rt"), "nested-cache")
			.unwrap();
		assert!(
			layer
				.redirect_unicode_cache(&cache_link.join("generated"))
				.is_err()
		);
		assert!(!source.path().join("generated").exists());
		platform::remove_layer(&cache_link);
	}

	#[test]
	fn deployment_refuses_a_nested_state_junction_before_writing_the_game() {
		use foch::plugin::{self, store::ArchiveEntry};
		let source = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		let manifest = plugin::Manifest::parse("schema=1\n[plugin]\nid=\"fixture\"\nname=\"Fixture\"\nversion=\"1.0.0\"\n[target]\ngame=\"eu4\"\nplatform=\"windows-x86_64\"\ngame_versions=\"*\"\nabi_major=1\n[entry]\nkind=\"native\"\nphase=\"entry\"\npath=\"fixture.dll\"").unwrap();
		let mut dll = vec![0; 0x80];
		dll[..2].copy_from_slice(b"MZ");
		dll[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
		dll[0x40..0x44].copy_from_slice(b"PE\0\0");
		dll[0x44..0x46].copy_from_slice(&plugin::store::MACHINE_AMD64.to_le_bytes());
		let package = plugin::store::adapt(
			manifest,
			vec![ArchiveEntry {
				path: "fixture.dll".into(),
				data: dll,
			}],
		)
		.unwrap();
		let store = output.path().join("store");
		plugin::install(&store, &package).unwrap();
		let game = plugin::GameIdentity {
			game: "eu4".into(),
			version: "1.37.5".parse().unwrap(),
			platform: plugin::WINDOWS_X64.into(),
		};
		let deployment = plugin::deployment::resolve(
			&game,
			&store,
			&[plugin::Selection {
				id: "fixture".into(),
				version: "1.0.0".parse().unwrap(),
				enabled: true,
				config: serde_json::json!({}),
			}],
		)
		.unwrap();
		let layer =
			RuntimeLayer::prepare(source.path(), &output.path().join("rt"), "state-junction")
				.unwrap();
		let data = output.path().join("data");
		fs::create_dir_all(data.join("plugins")).unwrap();
		let state = data.join("plugins/state");
		platform::link_dir(source.path(), &state).unwrap();
		assert!(
			deployment
				.stage(&layer.directory, &data, "state-junction", source.path())
				.is_err()
		);
		assert_eq!(fs::read_dir(source.path()).unwrap().count(), 0);
		platform::remove_layer(&state);
	}
}
