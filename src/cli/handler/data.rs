use crate::check::base_data::{
	BaseBuildObserver, BaseDataSource, build_base_snapshot_with_observer, default_release_tag,
	install_built_snapshot, install_snapshot_from_release, list_installed_base_data,
	resolve_game_root_and_version, write_release_artifacts, write_snapshot_bundle,
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
	if !args.install && args.output_dir.is_none() && !args.release_asset {
		return Err("请至少指定 --install、--output-dir 或 --release-asset".into());
	}

	let game = parse_game(&args.game_name)?;
	let resolved_version = if args.game_version.eq_ignore_ascii_case("auto") {
		None
	} else {
		Some(args.game_version.as_str())
	};
	let mut observer = BaseBuildObserver::stderr(game.key());
	let build = build_base_snapshot_with_observer(
		&game,
		&args.from_game_path,
		resolved_version,
		&mut observer,
	)
	.map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;

	observer
		.run_stage("write_outputs", |counts| {
			if args.install {
				let installed = install_built_snapshot(
					&build.snapshot,
					&build.encoded_snapshot,
					BaseDataSource::Build,
					Some(build.snapshot_asset_name.clone()),
					Some(build.snapshot_sha256.clone()),
				)?;
				counts.insert(
					"install_snapshot_bytes".to_string(),
					build.encoded_snapshot.len() as u64,
				);
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
					.ok_or_else(|| "使用 --release-asset 时必须提供 --output-dir".to_string())?;
				let release_output = write_release_artifacts(
					&build.snapshot,
					&build.encoded_snapshot,
					output_dir,
					&default_release_tag(),
				)?;
				let manifest_bytes = std::fs::metadata(&release_output.manifest_path)
					.map(|item| item.len())
					.unwrap_or_default();
				counts.insert(
					"release_snapshot_bytes".to_string(),
					build.encoded_snapshot.len() as u64,
				);
				counts.insert("release_manifest_bytes".to_string(), manifest_bytes);
				println!(
					"已写入 release 数据资产: snapshot={} manifest={}",
					release_output.snapshot_path.display(),
					release_output.manifest_path.display()
				);
			} else if let Some(output_dir) = args.output_dir.as_ref() {
				let bundle = write_snapshot_bundle(
					&build.snapshot,
					&build.encoded_snapshot,
					output_dir,
					BaseDataSource::Build,
					Some(build.snapshot_asset_name.clone()),
					Some(build.snapshot_sha256.clone()),
				)?;
				let metadata_bytes = std::fs::metadata(&bundle.metadata_path)
					.map(|item| item.len())
					.unwrap_or_default();
				counts.insert(
					"bundle_snapshot_bytes".to_string(),
					build.encoded_snapshot.len() as u64,
				);
				counts.insert("bundle_metadata_bytes".to_string(), metadata_bytes);
				println!(
					"已写入 snapshot bundle: snapshot={} metadata={}",
					bundle.snapshot_path.display(),
					bundle.metadata_path.display()
				);
			}
			Ok(())
		})
		.map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;

	let profile = observer.finish();
	if let Some(path) = args.profile_out.as_ref() {
		if let Some(parent) = path.parent()
			&& !parent.as_os_str().is_empty()
		{
			std::fs::create_dir_all(parent)?;
		}
		std::fs::write(path, serde_json::to_vec_pretty(&profile)?)?;
		println!("已写入 profiling 结果: {}", path.display());
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
