use crate::model::GamePathBuf;
use crate::playset::PlaysetEntry;
use crate::playset::descriptor::ModDescriptor;
use crate::playset::steam::WorkshopInstallIdentity;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct ModCandidate {
	pub entry: PlaysetEntry,
	pub mod_id: String,
	pub root_path: Option<PathBuf>,
	pub descriptor_path: Option<PathBuf>,
	pub descriptor: Option<ModDescriptor>,
	pub workshop_identity: Option<WorkshopInstallIdentity>,
	pub descriptor_error: Option<String>,
	/// The mod's inventory, relative to `root_path`, in byte order.
	pub files: Vec<GamePathBuf>,
}
