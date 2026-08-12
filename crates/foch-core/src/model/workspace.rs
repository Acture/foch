use crate::domain::descriptor::ModDescriptor;
use crate::domain::playlist::PlaylistEntry;
use crate::utils::steam::WorkshopInstallIdentity;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct ModCandidate {
	pub entry: PlaylistEntry,
	pub mod_id: String,
	pub root_path: Option<PathBuf>,
	pub descriptor_path: Option<PathBuf>,
	pub descriptor: Option<ModDescriptor>,
	pub workshop_identity: Option<WorkshopInstallIdentity>,
	pub descriptor_error: Option<String>,
	pub files: Vec<PathBuf>,
}
