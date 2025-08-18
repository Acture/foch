use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use typed_builder::TypedBuilder;

use log::debug;
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

pub trait FileWatcher {
	fn get_files(&self) -> &Arc<Mutex<HashMap<PathBuf, Option<[u8; 32]>>>>;
	fn watch(&mut self) -> Result<(), Box<dyn std::error::Error>>;
	fn unwatch(&mut self) -> Result<(), Box<dyn std::error::Error>>;
	fn update_hash(&mut self, path: &PathBuf) -> Result<(), Box<dyn std::error::Error>>;
}


mod arc_mutex_map_serde {
	use super::*;
	use serde::Deserializer;

	pub fn serialize<S>(
		map: &Arc<Mutex<HashMap<PathBuf, Option<[u8; 32]>>>>,
		serializer: S,
	) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		let guard = map.lock().map_err(serde::ser::Error::custom)?;
		let mut ser_map = serializer.serialize_map(Some(guard.len()))?;
		for (k, v) in guard.iter() {
			ser_map.serialize_entry(k, v)?;
		}
		ser_map.end()
	}

	pub fn deserialize<'de, D>(
        deserializer: D,
	) -> Result<Arc<Mutex<HashMap<PathBuf, Option<[u8; 32]>>>>, D::Error>
	where
		D: Deserializer<'de>,
	{
		struct MapVisitor;

		impl<'de> Visitor<'de> for MapVisitor {
			type Value = HashMap<PathBuf, Option<[u8; 32]>>;

			fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
				formatter.write_str("a map from PathBuf to Option<[u8; 32]>")
			}

			fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
			where
				M: MapAccess<'de>,
			{
				let mut map = HashMap::new();
				while let Some((key, value)) = access.next_entry()? {
					map.insert(key, value);
				}
				Ok(map)
			}
		}

		let raw = deserializer.deserialize_map(MapVisitor)?;
		Ok(Arc::new(Mutex::new(raw)))
	}
}

#[derive(Debug, Serialize, Deserialize, Default, TypedBuilder)]
pub struct FS {
	#[builder(default, setter(into, transform = |p: PathBuf| std::fs::canonicalize(p).expect("Path should be canonicalized")
	))]
	pub root: PathBuf,
	#[builder(default, setter(skip), setter(into))]
	#[serde(with = "arc_mutex_map_serde")]
	pub files: Arc<Mutex<HashMap<PathBuf, Option<[u8; 32]>>>>, // 相对路径 -> 文件Hash
	#[builder(default, setter(skip))]
	#[serde(skip)]
	watcher: Option<notify::RecommendedWatcher>,
}

impl FileWatcher for FS {
	fn get_files(&self) -> &Arc<Mutex<HashMap<PathBuf, Option<[u8; 32]>>>> {
		return &self.files
	}
	fn watch(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		// Watching the directory for changes and updating the files map
		if self.watcher.is_some() {
			return Err("Watcher already started".into());
		}

		let files_ref = Arc::clone(&self.files);
		let root = self.root.clone();

		let mut watcher = notify::recommended_watcher({
			move |res: Result<Event, notify::Error>| {
				match res {
					Ok(event) => {
						debug!("Filesystem event: {:?}", event);
						match event.kind {
							EventKind::Create(create_kind) => match create_kind {
								notify::event::CreateKind::File => {
									let path = event
										.paths
										.first()
										.expect("Event should have at least one path")
										.canonicalize()
										.expect("Path should be canonicalized")
										.strip_prefix(&root)
										.expect("Path should be relative to root")
										.to_path_buf();
									let hash = None;
									files_ref.lock().unwrap().entry(path).or_insert(hash);
								}
								_ => {
									debug!("Unsupported create kind: {:?}", create_kind);
								}
							},
							EventKind::Modify(modify_kind) => {
								match modify_kind {
									notify::event::ModifyKind::Data(_) => {
										let file = event
											.paths
											.first()
											.unwrap()
											.strip_prefix(&root)
											.expect("Path should be relative to root")
											.to_path_buf();
										let hash: Option<[u8; 32]> = None; // 这里可以计算文件的哈希值
										files_ref.lock().unwrap().insert(file, hash);
									}
									notify::event::ModifyKind::Name(_) => {
										let src = event
											.paths
											.first()
											.unwrap()
											.strip_prefix(&root)
											.expect("Path should be relative to root")
											.to_path_buf();
										files_ref.lock().unwrap().remove(&src);
										let dst = event
											.paths
											.get(1)
											.and_then(|p| p.strip_prefix(&root).ok())
											.map(PathBuf::from)
											.unwrap();
										files_ref.lock().unwrap().insert(dst, None);
									}
									_ => {
										debug!("Unsupported modify kind: {:?}", modify_kind);
									}
								}
							}
							_ => {
								debug!("Unsupported event kind: {:?}", event.kind);
							}
						}
					}
					Err(e) => panic!("watch error: {:?}", e),
				}
			}
		})?;

		watcher.watch(&self.root, RecursiveMode::Recursive)?;
		self.watcher = Some(watcher);
		Ok(())
	}

	fn unwatch(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		if let Some(mut watcher) = self.watcher.take() {
			watcher.unwatch(&self.root)?;
		}
		Ok(())
	}

	fn update_hash(&mut self, path: &PathBuf) -> Result<(), Box<dyn Error>> {
		self.files
			.lock()
			.map_err(|e| e.to_string())?
			.entry(path.clone())
			.and_modify(|hash| {
				// 这里可以计算文件的哈希值并更新
				todo!("Calculate hash for file: {:?}", path);
			})
			.or_insert(None);
		Ok(())
	}
}



#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	use std::time::{Duration, Instant};

	#[test]
	fn test_fs_watch() {
		let temp_dir = tempfile::tempdir().expect("Failed to create temporary directory");
		let mut fs = FS::builder().root(temp_dir.path().to_path_buf()).build();

		fs.watch().expect("Failed to start filesystem watcher");
		fs::write(temp_dir.path().join("test_file_1.txt"), b"Hello, World!")
			.expect("Failed to create test file");
		let start = Instant::now();
		let timeout = Duration::from_secs(3);
		let mut found = false;
		println!("Watching directory: {:?}", fs);

		while start.elapsed() < timeout {
			if fs
				.files
				.lock()
				.unwrap()
				.contains_key(&PathBuf::from("test_file_2.txt"))
			{
				found = true;
				break;
			}
			std::thread::sleep(Duration::from_millis(100));
		}

		assert!(found, "File was not found in the filesystem after creation");
		println!("{fs:?}")
	}
}
