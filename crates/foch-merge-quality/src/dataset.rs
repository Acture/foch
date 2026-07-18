use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::corpus::WorkshopProvenance;
use crate::object_store::TreeStats;

pub const SCHEMA: &str = "1.0.0";
pub const SCORER_VERSION: &str = "1.2.0";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
	Compatch,
	SourceMod,
	MergedOutput,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ObjectRecord {
	pub schema: String,
	pub object_id: String,
	pub kind: ObjectKind,
	pub content_hash: String,
	pub workshop_id: Option<String>,
	pub stats: TreeStats,
}

impl ObjectRecord {
	pub fn new(
		kind: ObjectKind,
		content_hash: String,
		workshop_id: Option<String>,
		stats: TreeStats,
	) -> Self {
		let kind_name = serde_json::to_string(&kind).expect("ObjectKind serializes");
		let workshop_id_part = workshop_id.as_deref().unwrap_or("");
		let object_id = stable_id(
			"object-record",
			&[
				kind_name.as_bytes(),
				workshop_id_part.as_bytes(),
				content_hash.as_bytes(),
			],
		);
		Self {
			schema: SCHEMA.to_string(),
			object_id,
			kind,
			content_hash,
			workshop_id,
			stats,
		}
	}
}

impl IdentifiedRecord for ObjectRecord {
	fn record_id(&self) -> &str {
		&self.object_id
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct GameIdentity {
	pub app_id: u32,
	pub version: String,
	pub steam_build_id: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct SnapshotObjectRef {
	pub workshop_id: String,
	pub content_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct SnapshotRecord {
	pub schema: String,
	pub snapshot_id: String,
	pub case_id: String,
	pub game: GameIdentity,
	pub compatch: SnapshotObjectRef,
	/// Source mods in declared playset order. Order is part of snapshot identity.
	pub source_mods: Vec<SnapshotObjectRef>,
}

impl SnapshotRecord {
	pub fn new(
		case_id: String,
		game: GameIdentity,
		compatch: SnapshotObjectRef,
		source_mods: Vec<SnapshotObjectRef>,
	) -> Self {
		let identity = serde_json::to_vec(&(&game, &compatch, &source_mods))
			.expect("snapshot identity serializes");
		let snapshot_id = stable_id("snapshot", &[case_id.as_bytes(), &identity]);
		Self {
			schema: SCHEMA.to_string(),
			snapshot_id,
			case_id,
			game,
			compatch,
			source_mods,
		}
	}
}

impl IdentifiedRecord for SnapshotRecord {
	fn record_id(&self) -> &str {
		&self.snapshot_id
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WorkshopObservation {
	pub workshop_id: String,
	pub title: String,
	pub time_created: i64,
	pub time_updated: i64,
	pub provenance: WorkshopProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ObservationRecord {
	pub schema: String,
	pub observation_id: String,
	pub snapshot_id: String,
	pub observed_at: String,
	pub compatch: WorkshopObservation,
	pub source_mods: Vec<WorkshopObservation>,
	pub subscriptions: i64,
	pub mod_churned: bool,
}

impl ObservationRecord {
	pub fn new(
		snapshot_id: String,
		observed_at: String,
		compatch: WorkshopObservation,
		source_mods: Vec<WorkshopObservation>,
		subscriptions: i64,
		mod_churned: bool,
	) -> Self {
		let payload = serde_json::to_vec(&(
			&snapshot_id,
			&observed_at,
			&compatch,
			&source_mods,
			subscriptions,
			mod_churned,
		))
		.expect("observation identity serializes");
		Self {
			schema: SCHEMA.to_string(),
			observation_id: stable_id("observation", &[&payload]),
			snapshot_id,
			observed_at,
			compatch,
			source_mods,
			subscriptions,
			mod_churned,
		}
	}
}

impl IdentifiedRecord for ObservationRecord {
	fn record_id(&self) -> &str {
		&self.observation_id
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
	Completed,
	MergeFailed,
	Crashed,
	TimedOut,
	Fatal,
}

impl TerminalStatus {
	pub fn counts_as_merge_failed(self) -> bool {
		self != Self::Completed
	}
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct MeasurementSummary {
	pub merge_status: Option<String>,
	pub ground_truth_files: usize,
	pub multi_source_files: usize,
	pub accepted_ground_truth_files: usize,
	pub accepted_multi_source_files: usize,
	pub all_ground_truth_verdicts: BTreeMap<String, usize>,
	pub multi_source_verdicts: BTreeMap<String, usize>,
	pub setup_ms: u64,
	pub merge_ms: u64,
	pub scoring_ms: u64,
	pub total_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct MeasurementRecord {
	pub schema: String,
	pub measurement_id: String,
	pub snapshot_id: String,
	pub executable_hash: String,
	pub scorer_version: String,
	pub config_hash: String,
	pub started_at: String,
	pub finished_at: String,
	pub status: TerminalStatus,
	pub detail: Option<String>,
	pub merged_output_hash: Option<String>,
	pub summary: Option<MeasurementSummary>,
}

pub struct MeasurementIdentity {
	pub snapshot_id: String,
	pub executable_hash: String,
	pub scorer_version: String,
	pub config_hash: String,
}

impl MeasurementRecord {
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		identity: MeasurementIdentity,
		started_at: String,
		finished_at: String,
		status: TerminalStatus,
		detail: Option<String>,
		merged_output_hash: Option<String>,
		summary: Option<MeasurementSummary>,
	) -> Self {
		let measurement_id = stable_id(
			"measurement",
			&[
				identity.snapshot_id.as_bytes(),
				identity.executable_hash.as_bytes(),
				identity.scorer_version.as_bytes(),
				identity.config_hash.as_bytes(),
			],
		);
		Self {
			schema: SCHEMA.to_string(),
			measurement_id,
			snapshot_id: identity.snapshot_id,
			executable_hash: identity.executable_hash,
			scorer_version: identity.scorer_version,
			config_hash: identity.config_hash,
			started_at,
			finished_at,
			status,
			detail,
			merged_output_hash,
			summary,
		}
	}
}

impl IdentifiedRecord for MeasurementRecord {
	fn record_id(&self) -> &str {
		&self.measurement_id
	}
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct FileResultRecord {
	pub schema: String,
	pub file_result_id: String,
	pub measurement_id: String,
	pub relative_path: String,
	pub result: serde_json::Value,
}

impl FileResultRecord {
	pub fn new(measurement_id: String, relative_path: String, result: serde_json::Value) -> Self {
		let file_result_id = stable_id(
			"file-result",
			&[measurement_id.as_bytes(), relative_path.as_bytes()],
		);
		Self {
			schema: SCHEMA.to_string(),
			file_result_id,
			measurement_id,
			relative_path,
			result,
		}
	}
}

impl IdentifiedRecord for FileResultRecord {
	fn record_id(&self) -> &str {
		&self.file_result_id
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatasetPaths {
	pub root: PathBuf,
	pub objects: PathBuf,
	pub work: PathBuf,
	pub object_records: PathBuf,
	pub snapshots: PathBuf,
	pub observations: PathBuf,
	pub measurements: PathBuf,
	pub file_results: PathBuf,
	pub shadow_measurements: PathBuf,
	pub annotations: PathBuf,
	pub manifest: PathBuf,
}

impl DatasetPaths {
	pub fn new(root: impl Into<PathBuf>) -> Self {
		let root = root.into();
		Self {
			objects: root.join("objects"),
			work: root.join(".work"),
			object_records: root.join("object_records.jsonl"),
			snapshots: root.join("snapshots.jsonl"),
			observations: root.join("observations.jsonl"),
			measurements: root.join("measurements.jsonl"),
			file_results: root.join("file_results.jsonl"),
			shadow_measurements: root.join("shadow_measurements.jsonl"),
			annotations: root.join("annotations.jsonl"),
			manifest: root.join("dataset.json"),
			root,
		}
	}

	pub fn ensure_layout(&self) -> io::Result<()> {
		fs::create_dir_all(&self.root)?;
		fs::create_dir_all(&self.objects)?;
		fs::create_dir_all(&self.work)?;
		for path in [
			&self.object_records,
			&self.snapshots,
			&self.observations,
			&self.measurements,
			&self.file_results,
			&self.shadow_measurements,
			&self.annotations,
		] {
			if !path.exists() {
				fs::write(path, b"")?;
			}
		}
		if !self.manifest.exists() {
			let manifest = serde_json::json!({
				"schema": SCHEMA,
				"format": "foch-merge-corpus"
			});
			fs::write(
				&self.manifest,
				format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
			)?;
		}
		Ok(())
	}
}

pub trait IdentifiedRecord {
	fn record_id(&self) -> &str;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppendOutcome {
	Inserted,
	AlreadyPresent,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AppendSummary {
	pub inserted: usize,
	pub already_present: usize,
}

pub fn append_unique<T>(path: &Path, record: &T) -> io::Result<AppendOutcome>
where
	T: DeserializeOwned + IdentifiedRecord + PartialEq + Serialize,
{
	let summary = append_unique_many(path, std::slice::from_ref(record))?;
	Ok(if summary.inserted == 1 {
		AppendOutcome::Inserted
	} else {
		AppendOutcome::AlreadyPresent
	})
}

/// Append a batch with one lock, one index scan, and one atomic rewrite. This
/// avoids quadratic I/O for per-file measurement records.
pub fn append_unique_many<T>(path: &Path, records: &[T]) -> io::Result<AppendSummary>
where
	T: DeserializeOwned + IdentifiedRecord + PartialEq + Serialize,
{
	if records.is_empty() {
		return Ok(AppendSummary::default());
	}
	let parent = path.parent().ok_or_else(|| {
		io::Error::new(
			ErrorKind::InvalidInput,
			format!("JSONL path has no parent: {}", path.display()),
		)
	})?;
	fs::create_dir_all(parent)?;
	let _lock = DatasetLock::acquire(parent)?;

	let existing = read_jsonl::<T>(path)?;
	let existing_by_id: HashMap<&str, &T> = existing
		.iter()
		.map(|record| (record.record_id(), record))
		.collect();
	let mut pending_by_id: HashMap<&str, &T> = HashMap::new();
	let mut inserted = Vec::new();
	let mut already_present = 0_usize;
	for record in records {
		if let Some(found) = existing_by_id.get(record.record_id()) {
			if *found == record {
				already_present += 1;
				continue;
			}
			return Err(io::Error::new(
				ErrorKind::AlreadyExists,
				format!(
					"record ID {} already exists with different content",
					record.record_id()
				),
			));
		}
		if let Some(found) = pending_by_id.get(record.record_id()) {
			if *found == record {
				already_present += 1;
				continue;
			}
			return Err(io::Error::new(
				ErrorKind::AlreadyExists,
				format!(
					"batch contains record ID {} with different content",
					record.record_id()
				),
			));
		}
		pending_by_id.insert(record.record_id(), record);
		inserted.push(record);
	}
	if inserted.is_empty() {
		return Ok(AppendSummary {
			inserted: 0,
			already_present,
		});
	}

	let mut output = if path.is_file() {
		fs::read(path)?
	} else {
		Vec::new()
	};
	if !output.is_empty() && !output.ends_with(b"\n") {
		return Err(io::Error::new(
			ErrorKind::InvalidData,
			format!(
				"JSONL file has an incomplete final line: {}",
				path.display()
			),
		));
	}
	for record in &inserted {
		serde_json::to_writer(&mut output, record).map_err(io::Error::other)?;
		output.push(b'\n');
	}
	atomic_write(path, &output)?;
	Ok(AppendSummary {
		inserted: inserted.len(),
		already_present,
	})
}

pub fn read_jsonl<T>(path: &Path) -> io::Result<Vec<T>>
where
	T: DeserializeOwned,
{
	if !path.exists() {
		return Ok(Vec::new());
	}
	let text = fs::read_to_string(path)?;
	text.lines()
		.enumerate()
		.filter(|(_, line)| !line.trim().is_empty())
		.map(|(index, line)| {
			serde_json::from_str(line).map_err(|err| {
				io::Error::new(
					ErrorKind::InvalidData,
					format!("{}:{}: {err}", path.display(), index + 1),
				)
			})
		})
		.collect()
}

pub fn stable_id(namespace: &str, parts: &[&[u8]]) -> String {
	let mut hasher = blake3::Hasher::new();
	hasher.update(&(namespace.len() as u64).to_le_bytes());
	hasher.update(namespace.as_bytes());
	for part in parts {
		hasher.update(&(part.len() as u64).to_le_bytes());
		hasher.update(part);
	}
	hasher.finalize().to_hex().to_string()
}

pub fn now_rfc3339() -> String {
	OffsetDateTime::now_utc()
		.format(&Rfc3339)
		.expect("RFC3339 formatting is infallible for UTC timestamps")
}

fn atomic_write(path: &Path, content: &[u8]) -> io::Result<()> {
	let parent = path.parent().expect("validated parent");
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_nanos();
	let temp = parent.join(format!(".jsonl-{}-{nanos}.tmp", std::process::id()));
	fs::write(&temp, content)?;
	fs::rename(&temp, path)
}

struct DatasetLock {
	path: PathBuf,
}

impl DatasetLock {
	fn acquire(root: &Path) -> io::Result<Self> {
		let path = root.join(".lock");
		let started = Instant::now();
		loop {
			match fs::create_dir(&path) {
				Ok(()) => return Ok(Self { path }),
				Err(err) if err.kind() == ErrorKind::AlreadyExists => {
					if started.elapsed() >= Duration::from_secs(30) {
						return Err(io::Error::new(
							ErrorKind::WouldBlock,
							format!("timed out waiting for dataset lock {}", path.display()),
						));
					}
					thread::sleep(Duration::from_millis(25));
				}
				Err(err) => return Err(err),
			}
		}
	}
}

impl Drop for DatasetLock {
	fn drop(&mut self) {
		let _ = fs::remove_dir(&self.path);
	}
}

#[cfg(test)]
mod tests {
	use serde::{Deserialize, Serialize};

	use super::*;

	#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
	struct Record {
		id: String,
		value: String,
	}

	impl IdentifiedRecord for Record {
		fn record_id(&self) -> &str {
			&self.id
		}
	}

	#[test]
	fn append_is_idempotent_and_rejects_id_collisions() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("records.jsonl");
		let record = Record {
			id: "same".to_string(),
			value: "first".to_string(),
		};
		assert_eq!(
			append_unique(&path, &record).unwrap(),
			AppendOutcome::Inserted
		);
		assert_eq!(
			append_unique(&path, &record).unwrap(),
			AppendOutcome::AlreadyPresent
		);
		let collision = Record {
			id: "same".to_string(),
			value: "different".to_string(),
		};
		assert_eq!(
			append_unique(&path, &collision).unwrap_err().kind(),
			ErrorKind::AlreadyExists
		);
		assert_eq!(read_jsonl::<Record>(&path).unwrap(), vec![record]);
	}

	#[test]
	fn batch_append_deduplicates_without_rewriting_per_record() {
		let temp = tempfile::tempdir().unwrap();
		let path = temp.path().join("records.jsonl");
		let records = vec![
			Record {
				id: "a".to_string(),
				value: "first".to_string(),
			},
			Record {
				id: "b".to_string(),
				value: "second".to_string(),
			},
		];
		assert_eq!(
			append_unique_many(&path, &records).unwrap(),
			AppendSummary {
				inserted: 2,
				already_present: 0,
			}
		);
		assert_eq!(
			append_unique_many(&path, &records).unwrap(),
			AppendSummary {
				inserted: 0,
				already_present: 2,
			}
		);
		assert_eq!(read_jsonl::<Record>(&path).unwrap(), records);
	}

	#[test]
	fn stable_ids_are_order_sensitive_and_namespace_scoped() {
		let ab = stable_id("snapshot", &[b"a", b"b"]);
		let ba = stable_id("snapshot", &[b"b", b"a"]);
		let measurement = stable_id("measurement", &[b"a", b"b"]);
		assert_ne!(ab, ba);
		assert_ne!(ab, measurement);
		assert_eq!(ab, stable_id("snapshot", &[b"a", b"b"]));
	}

	#[test]
	fn layout_initialization_is_repeatable() {
		let temp = tempfile::tempdir().unwrap();
		let paths = DatasetPaths::new(temp.path().join("dataset"));
		paths.ensure_layout().unwrap();
		paths.ensure_layout().unwrap();
		assert!(paths.objects.is_dir());
		assert!(paths.snapshots.is_file());
		let manifest: serde_json::Value =
			serde_json::from_str(&fs::read_to_string(paths.manifest).unwrap()).unwrap();
		assert_eq!(manifest["schema"], SCHEMA);
	}

	fn snapshot(source_ids: &[&str]) -> SnapshotRecord {
		SnapshotRecord::new(
			"case-1".to_string(),
			GameIdentity {
				app_id: 236850,
				version: "1.37.5".to_string(),
				steam_build_id: Some(42),
			},
			SnapshotObjectRef {
				workshop_id: "compatch".to_string(),
				content_hash: "c".repeat(64),
			},
			source_ids
				.iter()
				.map(|id| SnapshotObjectRef {
					workshop_id: (*id).to_string(),
					content_hash: id.repeat(64),
				})
				.collect(),
		)
	}

	#[test]
	fn snapshot_identity_is_repeatable_and_preserves_source_order() {
		let first = snapshot(&["a", "b"]);
		let repeated = snapshot(&["a", "b"]);
		let reordered = snapshot(&["b", "a"]);
		assert_eq!(first, repeated);
		assert_ne!(first.snapshot_id, reordered.snapshot_id);
	}

	#[test]
	fn measurement_identity_is_content_addressed() {
		let make = |started_at: &str| {
			MeasurementRecord::new(
				MeasurementIdentity {
					snapshot_id: "snapshot".to_string(),
					executable_hash: "executable".to_string(),
					scorer_version: SCORER_VERSION.to_string(),
					config_hash: "config".to_string(),
				},
				started_at.to_string(),
				"finished".to_string(),
				TerminalStatus::Completed,
				None,
				None,
				None,
			)
		};
		assert_eq!(make("first").measurement_id, make("later").measurement_id);
		assert!(TerminalStatus::TimedOut.counts_as_merge_failed());
		assert!(!TerminalStatus::Completed.counts_as_merge_failed());
	}
}
