//! Bundle-independent runtime layers shared by player and test launches.
//!
//! Game asset directories are shared through links; Foch writes into owned
//! runtime/state paths. Game and native plugin code retains the user's file
//! permissions, including writes through those shared links.

use crate::{layout, platform};
use foch::plugin::store;
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
		create_marker(&layer.directory.join("foch-runtime"))?;
		for entry in fs::read_dir(&game_root)? {
			let entry = entry?;
			let name = entry.file_name();
			let name = name.to_string_lossy();
			let is_dir = source_metadata(&entry.path())?.is_dir();
			if name.eq_ignore_ascii_case("gfx") && is_dir {
				layer.longest_relative = layer.longest_relative.max(deepest_relative(
					&entry.path(),
					name.encode_utf16().count(),
				)?);
				layer.prepare_gfx(&entry.path())?;
				continue;
			}
			match layout::classify_entry(&name, is_dir) {
				layout::LayerAction::Copy => {
					layer.longest_relative =
						layer.longest_relative.max(name.encode_utf16().count());
					store::copy_package_file(&entry.path(), &layer.directory.join(&*name))?;
				}
				layout::LayerAction::Link => {
					layer.longest_relative = layer.longest_relative.max(deepest_relative(
						&entry.path(),
						name.encode_utf16().count(),
					)?);
					platform::link_dir(&entry.path(), &layer.directory.join(&*name))?;
				}
				layout::LayerAction::Skip => layer.excluded.push(Excluded {
					path: name.into_owned(),
					sha256: if is_dir {
						None
					} else {
						Some(format!(
							"{:x}",
							Sha256::digest(store::read_package_file(&entry.path())?)
						))
					},
				}),
			}
		}
		fs::create_dir_all(layer.directory.join("gfx/fonts/eu4-unicode"))?;
		store::replace_package_file(&layer.directory.join("userdir.txt"), b"")?;
		store::replace_package_file(&layer.directory.join("steam_appid.txt"), b"236850")?;
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
			let metadata = source_metadata(&entry.path())?;
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
					if source_metadata(&font.path())?.is_dir() {
						platform::link_dir(&font.path(), &fonts.join(font.file_name()))?;
					} else {
						let copied = fonts.join(font.file_name());
						store::copy_package_file(&font.path(), &copied)?;
						let bytes = store::read_package_file(&copied)?;
						digest.update(font.file_name().to_string_lossy().as_bytes());
						digest.update(Sha256::digest(&bytes));
					}
				}
				self.fonts_sha256 = format!("{:x}", digest.finalize());
			} else if metadata.is_dir() {
				platform::link_dir(&entry.path(), &destination.join(name))?;
			} else {
				store::copy_package_file(&entry.path(), &destination.join(name))?;
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
		self.retain_after(|directory| Ok(directory.to_path_buf()))
	}

	/// Mark the layer before starting a player, then commit retention only when
	/// startup and its bookkeeping succeed. On error the caller must stop any
	/// child it started; dropping this layer then removes the failed preparation.
	pub fn retain_after<T>(&mut self, start: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
		create_marker(&self.directory.join("foch-player-runtime"))?;
		let result = start(&self.directory)?;
		self.remove_on_drop = false;
		Ok(result)
	}
}

/// Publish a fresh marker through an exclusively created file handle.
fn create_marker(path: &Path) -> io::Result<()> {
	store::create_package_file(path, b"1\n")
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

fn source_metadata(path: &Path) -> io::Result<fs::Metadata> {
	let metadata = fs::symlink_metadata(path)?;
	let linked = metadata.file_type().is_symlink();
	#[cfg(windows)]
	let linked = {
		use std::os::windows::fs::MetadataExt;
		linked || metadata.file_attributes() & 0x400 != 0
	};
	if linked {
		return Err(io::Error::other(format!(
			"source game entry cannot be linked: {}",
			path.display()
		)));
	}
	Ok(metadata)
}

fn deepest_relative(root: &Path, prefix: usize) -> io::Result<usize> {
	let mut deepest = prefix;
	for entry in fs::read_dir(root)? {
		let entry = entry?;
		let metadata = source_metadata(&entry.path())?;
		let child = prefix + 1 + entry.file_name().to_string_lossy().encode_utf16().count();
		deepest = deepest.max(if metadata.is_dir() {
			deepest_relative(&entry.path(), child)?
		} else {
			child
		});
	}
	Ok(deepest)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fresh_layers_exclude_runtime_metadata_and_preserve_source_entries() {
		for is_dir in [false, true] {
			let source = tempfile::tempdir().unwrap();
			let output = tempfile::tempdir().unwrap();
			let names = ["FOCH-RUNTIME", "Foch-Player-Runtime", "FOCH-HOST"];
			for name in names {
				let path = source.path().join(name);
				if is_dir {
					fs::create_dir(&path).unwrap();
					fs::write(path.join("original"), "unchanged").unwrap();
				} else {
					fs::write(path, "unchanged").unwrap();
				}
			}
			let layer = RuntimeLayer::prepare(source.path(), output.path(), "reserved").unwrap();
			assert_eq!(
				fs::read_to_string(layer.directory.join("foch-runtime")).unwrap(),
				"1\n"
			);
			assert!(!layer.directory.join("foch-player-runtime").exists());
			assert!(!layer.directory.join("foch-host").exists());
			for name in names {
				let excluded = layer
					.excluded
					.iter()
					.find(|entry| entry.path == name)
					.unwrap();
				assert_eq!(excluded.sha256.is_none(), is_dir);
				assert_eq!(
					fs::read_to_string(if is_dir {
						source.path().join(name).join("original")
					} else {
						source.path().join(name)
					})
					.unwrap(),
					"unchanged"
				);
			}
			let directory = layer.directory.clone();
			crate::sweep_runtime_base(output.path(), std::time::Duration::ZERO);
			assert!(
				!directory.exists(),
				"source player marker retained a test layer"
			);
			drop(layer);
		}
	}

	#[test]
	fn runtime_markers_refuse_existing_entries_without_truncating_external_files() {
		let scratch = tempfile::tempdir().unwrap();
		let original = scratch.path().join("original");
		fs::write(&original, "unchanged").unwrap();
		for name in ["foch-runtime", "foch-player-runtime"] {
			let path = scratch.path().join(name);
			fs::hard_link(&original, &path).unwrap();
			assert_eq!(
				create_marker(&path).unwrap_err().kind(),
				io::ErrorKind::AlreadyExists
			);
			assert_eq!(fs::read_to_string(&original).unwrap(), "unchanged");
			fs::remove_file(&path).unwrap();
			fs::write(&path, "existing marker").unwrap();
			assert_eq!(
				create_marker(&path).unwrap_err().kind(),
				io::ErrorKind::AlreadyExists
			);
			assert_eq!(fs::read_to_string(&path).unwrap(), "existing marker");
			fs::remove_file(&path).unwrap();
			create_marker(&path).unwrap();
			assert_eq!(fs::read_to_string(&path).unwrap(), "1\n");
		}
		let source = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		let mut layer = RuntimeLayer::prepare(source.path(), output.path(), "marker").unwrap();
		let directory = layer.directory.clone();
		fs::hard_link(&original, directory.join("foch-player-runtime")).unwrap();
		let result: io::Result<()> =
			layer.retain_after(|_| panic!("existing marker must prevent startup"));
		assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
		drop(layer);
		assert!(!directory.exists());
		assert_eq!(fs::read_to_string(&original).unwrap(), "unchanged");
	}

	#[test]
	fn source_directory_links_are_refused_without_traversal_or_source_changes() {
		for relative in [
			"assets/nested-link",
			"gfx/fonts/nested-link",
			"top-link",
			"assets/loop",
		] {
			let source = tempfile::tempdir().unwrap();
			let outside = tempfile::tempdir().unwrap();
			let output = tempfile::tempdir().unwrap();
			fs::create_dir_all(source.path().join("assets")).unwrap();
			fs::create_dir_all(source.path().join("gfx/fonts")).unwrap();
			fs::write(outside.path().join("original"), "untouched").unwrap();
			let link = source.path().join(relative);
			let target = if relative.ends_with("loop") {
				source.path().join("assets")
			} else {
				outside.path().to_owned()
			};
			platform::link_dir(&target, &link).unwrap();
			let result = RuntimeLayer::prepare(source.path(), output.path(), "linked-source");
			platform::remove_layer(&link);
			let error = match result {
				Ok(layer) => {
					drop(layer);
					panic!("accepted source directory link {relative}")
				}
				Err(error) => error,
			};
			assert!(
				error
					.to_string()
					.contains("source game entry cannot be linked"),
				"{relative}: {error}"
			);
			assert!(!output.path().join("linked-source").exists());
			assert_eq!(
				fs::read(outside.path().join("original")).unwrap(),
				b"untouched"
			);
		}
	}

	#[test]
	fn non_ascii_game_directories_use_windows_path_units() {
		let source = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		fs::create_dir_all(source.path().join("目录😀/nested")).unwrap();
		fs::write(source.path().join("目录😀/nested/file"), "resource").unwrap();
		let layer = RuntimeLayer::prepare(source.path(), output.path(), "utf16").unwrap();
		// Two BMP characters and one surrogate pair, plus /nested/file.
		assert_eq!(layer.longest_relative, 4 + 12);
		assert_eq!(
			fs::read(layer.directory.join("目录😀/nested/file")).unwrap(),
			b"resource"
		);
	}

	#[test]
	fn copied_files_obey_the_utf16_path_budget_and_failed_layers_are_removed() {
		let source = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		fs::create_dir(source.path().join("assets")).unwrap();
		fs::write(source.path().join("assets/original"), "shared resource").unwrap();
		let directory = PathBuf::from(
			foch::plugin::deployment::path_text(&output.path().canonicalize().unwrap()).unwrap(),
		)
		.join("within");
		let relative_len =
			layout::MAX_PATH - directory.to_string_lossy().encode_utf16().count() - 2;
		let filename = format!("{}😀.txt", "x".repeat(relative_len - 6));
		fs::write(source.path().join(&filename), "copied resource").unwrap();
		let layer = RuntimeLayer::prepare(source.path(), output.path(), "within").unwrap();
		assert_eq!(
			fs::read(layer.directory.join(&filename)).unwrap(),
			b"copied resource"
		);
		assert!(layer.budget_error().is_none());
		drop(layer);
		assert!(!output.path().join("within").exists());

		let too_long = format!("{filename}x");
		fs::rename(source.path().join(&filename), source.path().join(&too_long)).unwrap();
		let result = RuntimeLayer::prepare(source.path(), output.path(), "beyond");
		let error = match result {
			Ok(layer) => {
				drop(layer);
				panic!("accepted a copied file beyond the classic Windows path budget")
			}
			Err(error) => error,
		};
		assert!(error.to_string().contains("too long"), "{error}");
		assert!(!output.path().join("beyond").exists());
		assert_eq!(
			fs::read(source.path().join(&too_long)).unwrap(),
			b"copied resource"
		);
		assert_eq!(
			fs::read(source.path().join("assets/original")).unwrap(),
			b"shared resource"
		);
		fs::rename(source.path().join(&too_long), source.path().join(&filename)).unwrap();
		let retry = RuntimeLayer::prepare(source.path(), output.path(), "beyond").unwrap();
		assert_eq!(
			fs::read(retry.directory.join(&filename)).unwrap(),
			b"copied resource"
		);
		drop(retry);
		assert!(!output.path().join("beyond").exists());
	}

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
				.as_deref(),
			Some("9aa2bd801c7163b8dc8a6668b43928b3ad16a19124f4e3e8dfb3e0db6ca607d8")
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
	fn copied_font_permissions_and_digest_match_the_source() {
		let source = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		fs::create_dir_all(source.path().join("gfx/fonts")).unwrap();
		let font = source.path().join("gfx/fonts/base.fnt");
		fs::write(&font, "original font").unwrap();
		let original = fs::metadata(&font).unwrap().permissions();
		#[cfg(unix)]
		let permissions = {
			use std::os::unix::fs::PermissionsExt;
			fs::Permissions::from_mode(0o640)
		};
		#[cfg(windows)]
		let permissions = {
			let mut permissions = original.clone();
			permissions.set_readonly(true);
			permissions
		};
		fs::set_permissions(&font, permissions.clone()).unwrap();
		let expected_permissions = fs::metadata(&font).unwrap().permissions();
		let prepared = RuntimeLayer::prepare(source.path(), output.path(), "font-mode");
		let layer = prepared.unwrap();
		let copied = layer.directory.join("gfx/fonts/base.fnt");
		let copied_permissions = fs::metadata(&copied).unwrap().permissions();
		let copied_bytes = fs::read(&copied).unwrap();
		let directory = layer.directory.clone();
		let fonts_sha256 = layer.fonts_sha256.clone();
		drop(layer);
		let source_permissions = fs::metadata(&font).unwrap().permissions();
		// Restore only the disposable source fixture for tempfile cleanup.
		fs::set_permissions(&font, original.clone()).unwrap();
		let remained = directory.exists();
		if remained {
			// Keep a failing cleanup regression from leaking this owned fixture.
			fs::set_permissions(&copied, original).unwrap();
			platform::remove_layer(&directory);
		}
		let mut digest = Sha256::new();
		digest.update(b"base.fnt");
		digest.update(Sha256::digest(b"original font"));
		assert_eq!(fonts_sha256, format!("{:x}", digest.finalize()));
		assert_eq!(copied_bytes, b"original font");
		assert_eq!(fs::read(&font).unwrap(), b"original font");
		assert_eq!(source_permissions, expected_permissions);
		assert_eq!(copied_permissions, expected_permissions);
		assert!(!remained, "dropping a layer left its read-only font behind");
	}

	#[test]
	fn player_startup_is_marked_before_handoff_and_failed_starts_are_removed() {
		let source = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		let mut layer = RuntimeLayer::prepare(source.path(), output.path(), "handoff").unwrap();
		let directory = layer.directory.clone();
		let result: io::Result<()> = layer.retain_after(|directory| {
			assert!(directory.join("foch-player-runtime").is_file());
			crate::sweep_runtime_base(output.path(), std::time::Duration::ZERO);
			assert!(directory.is_dir());
			Err(io::Error::other("startup or PID bookkeeping failed"))
		});
		assert!(result.is_err());
		drop(layer);
		assert!(!directory.exists());

		let mut layer = RuntimeLayer::prepare(source.path(), output.path(), "handoff").unwrap();
		fs::create_dir(directory.join("foch-player-runtime")).unwrap();
		let result: io::Result<()> =
			layer.retain_after(|_| panic!("marker failure must prevent startup"));
		assert!(result.is_err());
		drop(layer);
		assert!(!directory.exists());

		let mut layer = RuntimeLayer::prepare(source.path(), output.path(), "handoff").unwrap();
		layer.retain_after(|_| Ok(())).unwrap();
		drop(layer);
		crate::sweep_runtime_base(output.path(), std::time::Duration::ZERO);
		assert!(directory.is_dir());
		platform::remove_layer(&directory);
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
		// An unresolved link is an existing directory entry, not a missing
		// component whose outside parent can authorize later writes.
		let target = source.path().join("pending-target");
		let moved = source.path().join("moved-target");
		fs::create_dir(&target).unwrap();
		fs::write(target.join("original"), "unchanged").unwrap();
		let dangling = output.path().join("dangling");
		platform::link_dir(&target, &dangling).unwrap();
		fs::rename(&target, &moved).unwrap();
		assert!(!dangling.exists());
		assert!(fs::symlink_metadata(&dangling).is_ok());
		for destination in [
			dangling.clone(),
			dangling.join("child"),
			dangling.join("child/nested"),
		] {
			assert!(
				ensure_outside_game(source.path(), &destination).is_err(),
				"{}",
				destination.display()
			);
		}
		assert!(!target.exists());
		assert_eq!(
			fs::read_to_string(moved.join("original")).unwrap(),
			"unchanged"
		);
		platform::remove_layer(&dangling);
		let ordinary_missing = output.path().join("missing/nested");
		ensure_outside_game(source.path(), &ordinary_missing).unwrap();
		assert!(!output.path().join("missing").exists());
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
		// A user-selected base may alias an outside directory. Ownership begins
		// at the fresh run directory, never at the base or an existing run.
		let external = tempfile::tempdir().unwrap();
		fs::write(external.path().join("original"), "unchanged").unwrap();
		let base_alias = output.path().join("external-base");
		platform::link_dir(external.path(), &base_alias).unwrap();
		let owned = RuntimeLayer::prepare(source.path(), &base_alias, "owned-run").unwrap();
		assert_eq!(
			owned.directory.canonicalize().unwrap(),
			external.path().join("owned-run").canonicalize().unwrap()
		);
		fs::write(owned.directory.join("sentinel"), "existing run").unwrap();
		assert!(RuntimeLayer::prepare(source.path(), &base_alias, "owned-run").is_err());
		assert_eq!(
			fs::read_to_string(owned.directory.join("sentinel")).unwrap(),
			"existing run"
		);
		drop(owned);
		assert!(!external.path().join("owned-run").exists());
		assert_eq!(
			fs::read_to_string(external.path().join("original")).unwrap(),
			"unchanged"
		);
		assert!(base_alias.exists());
		platform::remove_layer(&base_alias);
	}

	#[test]
	fn installed_store_directory_links_cannot_import_external_artifacts() {
		use foch::plugin::{self, store::ArchiveEntry};
		let outside = tempfile::tempdir().unwrap();
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
				data: dll.clone(),
			}],
		)
		.unwrap();
		let installed = match plugin::install(outside.path(), &package).unwrap() {
			plugin::store::Installed::New(path) => path,
			_ => unreachable!(),
		};
		let mut changed_dll = dll.clone();
		changed_dll[0x7f] = 1;
		let changed_package = plugin::store::adapt(
			package.manifest.clone(),
			vec![ArchiveEntry {
				path: "fixture.dll".into(),
				data: changed_dll,
			}],
		)
		.unwrap();
		for link_plugin_directory in [true, false] {
			let store = tempfile::tempdir().unwrap();
			let plugin_dir = store
				.path()
				.join(installed.parent().unwrap().file_name().unwrap());
			let link = if link_plugin_directory {
				platform::link_dir(installed.parent().unwrap(), &plugin_dir).unwrap();
				plugin_dir
			} else {
				fs::create_dir(&plugin_dir).unwrap();
				let link = plugin_dir.join(installed.file_name().unwrap());
				platform::link_dir(&installed, &link).unwrap();
				link
			};
			let changed_import =
				link_plugin_directory.then(|| plugin::install(store.path(), &changed_package));
			let repeated_import = plugin::install(store.path(), &package);
			let (versions, problems) = plugin::store::installed_versions(store.path());
			let deployment = plugin::deployment::resolve(
				&plugin::GameIdentity {
					game: "eu4".into(),
					version: "1.37.5".parse().unwrap(),
					platform: plugin::WINDOWS_X64.into(),
				},
				store.path(),
				&[plugin::Selection {
					id: "fixture".into(),
					version: "1.0.0".parse().unwrap(),
					enabled: true,
					config: serde_json::json!({}),
				}],
			);
			platform::remove_layer(&link);
			if let Some(import) = changed_import {
				assert!(
					import.is_err(),
					"import wrote through a plugin directory link"
				);
			}
			assert!(
				repeated_import.is_err(),
				"linked version accepted as installed"
			);
			assert!(versions.is_empty(), "external artifact accepted");
			assert!(!problems.is_empty());
			assert!(deployment.is_err());
			assert_eq!(fs::read(installed.join("fixture.dll")).unwrap(), dll);
			assert_eq!(
				fs::read_dir(installed.parent().unwrap()).unwrap().count(),
				1
			);
		}
	}

	#[test]
	fn derived_store_links_cannot_redirect_imports_into_source_directories() {
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
		for (index, relative) in ["plugins", "plugins/store"].iter().enumerate() {
			let data = output.path().join(format!("data-{index}"));
			let link = data.join(relative);
			fs::create_dir_all(link.parent().unwrap()).unwrap();
			platform::link_dir(source.path(), &link).unwrap();
			let result = plugin::paths::prepare_store_root(&data)
				.and_then(|root| plugin::install(&root, &package));
			platform::remove_layer(&link);
			assert!(
				result.is_err(),
				"{relative} redirected an import into a source directory"
			);
			assert_eq!(fs::read_dir(source.path()).unwrap().count(), 0);
		}
		let alias_target = tempfile::tempdir().unwrap();
		let data_alias = output.path().join("data-alias");
		platform::link_dir(alias_target.path(), &data_alias).unwrap();
		let store_root = plugin::paths::prepare_store_root(&data_alias).unwrap();
		plugin::install(&store_root, &package).unwrap();
		assert!(
			store_root
				.canonicalize()
				.unwrap()
				.starts_with(alias_target.path().canonicalize().unwrap())
		);
		assert_eq!(plugin::store::installed_versions(&store_root).0.len(), 1);
		platform::remove_layer(&data_alias);
	}

	#[test]
	fn derived_store_reads_refuse_internal_links_without_creating_missing_directories() {
		use foch::plugin::{self, store::ArchiveEntry};
		let outside = tempfile::tempdir().unwrap();
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
				data: dll.clone(),
			}],
		)
		.unwrap();
		let real_store = plugin::paths::prepare_store_root(outside.path()).unwrap();
		let installed = plugin::store::version_dir(&real_store, &package);
		plugin::install(&real_store, &package).unwrap();
		for (index, relative) in ["plugins", "plugins/store"].iter().enumerate() {
			let data = output.path().join(format!("read-{index}"));
			let link = data.join(relative);
			fs::create_dir_all(link.parent().unwrap()).unwrap();
			platform::link_dir(&outside.path().join(relative), &link).unwrap();
			let result =
				plugin::paths::checked_store_root(&data).map(|root| plugin::store::catalog(&root));
			platform::remove_layer(&link);
			assert!(result.is_err(), "{relative} read an external plugin store");
			assert_eq!(fs::read(installed.join("fixture.dll")).unwrap(), dll);
		}
		for existing_root in [false, true] {
			let data = output.path().join(format!("empty-{existing_root}"));
			if existing_root {
				fs::create_dir(&data).unwrap();
			}
			let root = plugin::paths::checked_store_root(&data).unwrap();
			assert!(plugin::store::catalog(&root).0.is_empty());
			assert_eq!(data.exists(), existing_root);
			assert!(!data.join("plugins").exists());
		}
		let alias = output.path().join("selected-data-root");
		platform::link_dir(outside.path(), &alias).unwrap();
		let root = plugin::paths::checked_store_root(&alias).unwrap();
		assert!(plugin::store::catalog(&root).0.contains_key("fixture"));
		let entries_before = fs::read_dir(outside.path()).unwrap().count();
		let missing = alias.join("missing-data");
		let root = plugin::paths::checked_store_root(&missing).unwrap();
		assert!(plugin::store::catalog(&root).0.is_empty());
		assert!(!missing.exists());
		assert_eq!(
			fs::read_dir(outside.path()).unwrap().count(),
			entries_before
		);
		assert_eq!(fs::read(installed.join("fixture.dll")).unwrap(), dll);
		platform::remove_layer(&alias);
	}

	#[test]
	fn deployment_refuses_a_nested_state_junction_before_writing_the_game() {
		use foch::plugin::{self, store::ArchiveEntry};
		let source = tempfile::tempdir().unwrap();
		let output = tempfile::tempdir().unwrap();
		let manifest = plugin::Manifest::parse("schema=1\n[plugin]\nid=\"fixture\"\nname=\"Fixture\"\nversion=\"1.0.0\"\n[target]\ngame=\"eu4\"\nplatform=\"windows-x86_64\"\ngame_versions=\"*\"\nabi_major=1\n[entry]\nkind=\"native\"\nphase=\"entry\"\npath=\"fixture.dll\"").unwrap();
		let mut manifest = manifest;
		manifest.writes.push(plugin::manifest::WriteKind::FontCache);
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
		let outside = tempfile::tempdir().unwrap();
		fs::write(outside.path().join("sentinel"), "unchanged").unwrap();
		let state_leaf = format!(
			"plugins/state/{}",
			&plugin::deployment::hash(b"fixture")[..12]
		);
		let cache_leaf = format!("plugins/cache/{}", package.digest);
		for (index, relative) in [
			"plugins".into(),
			"plugins/state".into(),
			"plugins/cache".into(),
			state_leaf,
			cache_leaf,
		]
		.iter()
		.enumerate()
		{
			let data = output.path().join(format!("data-{index}"));
			let link = data.join(relative);
			fs::create_dir_all(link.parent().unwrap()).unwrap();
			platform::link_dir(outside.path(), &link).unwrap();
			let run_id = format!("outside-data-{index}");
			let layer =
				RuntimeLayer::prepare(source.path(), &output.path().join("rt"), &run_id).unwrap();
			let result = deployment.stage(&layer.directory, &data, &run_id, source.path());
			platform::remove_layer(&link);
			assert!(result.is_err(), "{relative} followed an outside link");
			assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
			assert_eq!(
				fs::read_to_string(outside.path().join("sentinel")).unwrap(),
				"unchanged"
			);
		}
		let baseline = deployment
			.prepare_font_cache(
				&output.path().join("font-baseline"),
				"base-fonts",
				source.path(),
			)
			.unwrap()
			.unwrap();
		let font_leaf = Path::new("plugins/font-cache").join(baseline.file_name().unwrap());
		let font_leaf = font_leaf.to_string_lossy().into_owned();
		for (index, relative) in ["plugins".into(), "plugins/font-cache".into(), font_leaf]
			.iter()
			.enumerate()
		{
			let data = output.path().join(format!("font-data-{index}"));
			let link = data.join(relative);
			fs::create_dir_all(link.parent().unwrap()).unwrap();
			platform::link_dir(outside.path(), &link).unwrap();
			let result = deployment.prepare_font_cache(&data, "base-fonts", source.path());
			platform::remove_layer(&link);
			assert!(
				result.is_err(),
				"{relative} followed an outside font-cache link"
			);
			assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
		}
		// A selected root alias is supported, while its children remain owned.
		let aliased_root = output.path().join("data-alias");
		let alias_target = tempfile::tempdir().unwrap();
		platform::link_dir(alias_target.path(), &aliased_root).unwrap();
		let layer = RuntimeLayer::prepare(source.path(), &output.path().join("rt"), "aliased-data")
			.unwrap();
		// Host wire plans use Windows paths; directory/cache checks are portable.
		#[cfg(windows)]
		{
			let prepared = deployment
				.stage(
					&layer.directory,
					&aliased_root,
					"aliased-data",
					source.path(),
				)
				.unwrap();
			for kind in ["data", "cache"] {
				assert!(
					Path::new(&prepared.plan.plugins[0].dirs[kind])
						.canonicalize()
						.unwrap()
						.starts_with(alias_target.path().canonicalize().unwrap())
				);
			}
		}
		let font_cache = deployment
			.prepare_font_cache(&aliased_root, "base-fonts", source.path())
			.unwrap()
			.unwrap();
		assert!(font_cache.starts_with(alias_target.path().canonicalize().unwrap()));
		layer.redirect_unicode_cache(&font_cache).unwrap();
		fs::write(
			layer.directory.join("gfx/fonts/eu4-unicode/generated"),
			"font cache",
		)
		.unwrap();
		assert_eq!(
			fs::read_to_string(font_cache.join("generated")).unwrap(),
			"font cache"
		);
		assert_eq!(fs::read_dir(source.path()).unwrap().count(), 0);
		platform::remove_layer(&aliased_root);
		let marked_outside = output.path().join("marked-outside");
		fs::create_dir(&marked_outside).unwrap();
		fs::write(marked_outside.join("foch-runtime"), "fixture marker").unwrap();
		let runtime_link = output.path().join("runtime-link");
		platform::link_dir(&marked_outside, &runtime_link).unwrap();
		let result = deployment.stage(&runtime_link, &data, "linked-runtime", source.path());
		platform::remove_layer(&runtime_link);
		assert!(result.is_err(), "linked runtime accepted for staging");
		assert_eq!(fs::read_dir(&marked_outside).unwrap().count(), 1);
		fs::write(source.path().join("foch-runtime"), "fixture marker").unwrap();
		assert!(
			deployment
				.stage(source.path(), &data, "inside-game", source.path())
				.is_err()
		);
		platform::link_dir(source.path(), &runtime_link).unwrap();
		let result = deployment.stage(&runtime_link, &data, "linked-game", source.path());
		platform::remove_layer(&runtime_link);
		assert!(result.is_err());
		assert_eq!(fs::read_dir(source.path()).unwrap().count(), 1);
		assert_eq!(
			fs::read_to_string(source.path().join("foch-runtime")).unwrap(),
			"fixture marker"
		);
	}
}
