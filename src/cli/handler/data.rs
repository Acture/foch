use crate::check::base_data::{
	BaseDataSource, build_base_snapshot, default_release_tag, install_built_snapshot,
	install_snapshot_from_release, list_installed_base_data, resolve_game_root_and_version,
	write_release_artifacts, write_snapshot_bundle,
};
use crate::cli::arg::{
	DataArgs, DataBuildArgs, DataInstallArgs, DataListArgs, FochCliDataCommands,
};
use crate::cli::config::Config;
use crate::cli::handler::HandlerResult;
use crate::domain::game::Game;

pub fn handle_data(data_args: &DataArgs, config: Config) -> HandlerResult {
	match &data_args.command {
		FochCliDataCommands::Install(args) => handle_data_install(args, config),
		FochCliDataCommands::Build(args) => handle_data_build(args),
		FochCliDataCommands::List(args) => handle_data_list(args),
	}
}

fn handle_data_install(args: &DataInstallArgs, config: Config) -> HandlerResult {
	let game = parse_game(&args.game_name)?;
	let resolved_version = if args.game_version.eq_ignore_ascii_case("auto") {
		let (_game_root, version) = resolve_game_root_and_version(&config, &game)
			.map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;
		version
	} else {
		args.game_version.clone()
	};
	let installed =
		install_snapshot_from_release(&game, &resolved_version, args.release_tag.as_deref())
			.map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;
	println!(
		"已安装基础数据: game={} version={} source=download path={}",
		installed.metadata.game,
		installed.metadata.game_version,
		installed.install_dir.display()
	);
	Ok(0)
}

fn handle_data_build(args: &DataBuildArgs) -> HandlerResult {
	let game = parse_game(&args.game_name)?;
	let resolved_version = if args.game_version.eq_ignore_ascii_case("auto") {
		None
	} else {
		Some(args.game_version.as_str())
	};
	let build = build_base_snapshot(&game, &args.from_game_path, resolved_version)
		.map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;

	if !args.install && args.output_dir.is_none() && !args.release_asset {
		return Err("请至少指定 --install、--output-dir 或 --release-asset".into());
	}

	if args.install {
		let installed = install_built_snapshot(
			&build.snapshot,
			BaseDataSource::Build,
			Some(build.snapshot_asset_name.clone()),
			Some(build.snapshot_sha256.clone()),
		)
		.map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;
		println!(
			"已构建并安装基础数据: game={} version={} path={}",
			installed.metadata.game,
			installed.metadata.game_version,
			installed.install_dir.display()
		);
	}

	if args.release_asset {
		let output_dir = args
			.output_dir
			.as_ref()
			.ok_or_else(|| "使用 --release-asset 时必须提供 --output-dir".to_string())
			.map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;
		let release_output =
			write_release_artifacts(&build.snapshot, output_dir, &default_release_tag())
				.map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;
		println!(
			"已写入 release 数据资产: snapshot={} manifest={}",
			release_output.snapshot_path.display(),
			release_output.manifest_path.display()
		);
	} else if let Some(output_dir) = args.output_dir.as_ref() {
		let bundle = write_snapshot_bundle(
			&build.snapshot,
			output_dir,
			BaseDataSource::Build,
			Some(build.snapshot_asset_name.clone()),
			Some(build.snapshot_sha256.clone()),
		)
		.map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;
		println!(
			"已写入 snapshot bundle: snapshot={} metadata={}",
			bundle.snapshot_path.display(),
			bundle.metadata_path.display()
		);
	}

	Ok(0)
}

fn handle_data_list(args: &DataListArgs) -> HandlerResult {
	let entries =
		list_installed_base_data().map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;
	if args.json {
		println!("{}", serde_json::to_string_pretty(&entries)?);
		return Ok(0);
	}

	if entries.is_empty() {
		println!("No installed base data");
		return Ok(0);
	}

	for entry in entries {
		println!(
			"game={} version={} schema={} source={:?} path={}",
			entry.game, entry.game_version, entry.schema_version, entry.source, entry.install_path
		);
	}

	Ok(0)
}

fn parse_game(value: &str) -> Result<Game, Box<dyn std::error::Error>> {
	Game::from_key(value).ok_or_else(|| format!("不支持的游戏标识: {value}").into())
}
