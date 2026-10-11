//! Engine host windows documented by EU4's `common/custom_gui/example.txt`.

/// What `ROOT` is inside a documented host. `FROM` is always the clicking
/// country, so `Actor` means `ROOT = FROM`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum HostRoot {
	Actor,
	ClickedProvince,
	SelectedCountry,
	TradeNodeCapital,
}

/// Scripted GUI objects run only under a descendant of one of these windows.
/// The table copies the installed game's documentation, including its
/// `countrydiplimacyview` spelling. "All faction windows" are omitted because
/// the documentation does not name them.
const DOCUMENTED_HOSTS: &[(&str, &str, HostRoot)] = &[
	(
		"interface/provinceview.gui",
		"province_window",
		HostRoot::ClickedProvince,
	),
	(
		"interface/provinceview.gui",
		"buildings_window",
		HostRoot::ClickedProvince,
	),
	(
		"interface/provinceview.gui",
		"state_window",
		HostRoot::ClickedProvince,
	),
	(
		"interface/countrycourtview.gui",
		"countrycourtview",
		HostRoot::Actor,
	),
	(
		"interface/countrygovernmentview.gui",
		"countrygovernmentview",
		HostRoot::Actor,
	),
	(
		"interface/countrydiplomacyview.gui",
		"countrydiplimacyview",
		HostRoot::SelectedCountry,
	),
	(
		"interface/countryeconomyview.gui",
		"countryeconomyview",
		HostRoot::Actor,
	),
	(
		"interface/countrytradeview.gui",
		"countrytradeview",
		HostRoot::Actor,
	),
	(
		"interface/countrytechnologyview.gui",
		"countrytechnologyview",
		HostRoot::Actor,
	),
	(
		"interface/countryideasview.gui",
		"countryideasview",
		HostRoot::Actor,
	),
	(
		"interface/countrymissionsview.gui",
		"countrymissionsview",
		HostRoot::SelectedCountry,
	),
	(
		"interface/countrydecisionview.gui",
		"countrydecisionsview",
		HostRoot::Actor,
	),
	(
		"interface/countrystabilityview.gui",
		"countrystabilityview",
		HostRoot::Actor,
	),
	(
		"interface/countryreligionview.gui",
		"countryreligionview",
		HostRoot::Actor,
	),
	(
		"interface/countrymilitaryview.gui",
		"countrymilitaryview",
		HostRoot::Actor,
	),
	(
		"interface/countrysubjectsview.gui",
		"countrysubjectview",
		HostRoot::Actor,
	),
	(
		"interface/countryestatesview.gui",
		"countryestatesview",
		HostRoot::Actor,
	),
	("interface/ages_view.gui", "ages_view", HostRoot::Actor),
	(
		"interface/tradeinterface.gui",
		"TradeNodeInterface",
		HostRoot::TradeNodeCapital,
	),
	("interface/hre.gui", "hre_window", HostRoot::Actor),
	("interface/papacy.gui", "papacy_window", HostRoot::Actor),
	(
		"interface/celestialempireview.gui",
		"celestial_window",
		HostRoot::Actor,
	),
	(
		"interface/countrynativesview.gui",
		"natives_window",
		HostRoot::Actor,
	),
	(
		"interface/religiousreforms.gui",
		"reforms_window",
		HostRoot::Actor,
	),
	(
		"interface/parliament.gui",
		"parliament_window",
		HostRoot::Actor,
	),
];

/// The documented `ROOT` of `window` when it is defined in `gui_file`.
pub fn documented_host(gui_file: &str, window: &str) -> Option<HostRoot> {
	let gui_file = gui_file.replace('\\', "/");
	DOCUMENTED_HOSTS
		.iter()
		.find(|(file, name, _)| file.eq_ignore_ascii_case(&gui_file) && *name == window)
		.map(|(_, _, root)| *root)
}
