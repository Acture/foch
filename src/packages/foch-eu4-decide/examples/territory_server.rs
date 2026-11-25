//! The territorial war as an environment server.
//!
//! `cargo run -p foch-eu4-decide --example territory_server -- [--scenarios N --rollouts K]`
//!
//! Same stdio protocol as env_server: one JSON state line per decision
//! point, one `{"done":true}` per finished episode. The state carries the
//! board (province nodes), adjacency, rules, and candidates.
//!
//! `--oracle-labels K` (default 0) tags K decision points per played
//! episode with the rollout oracle: those state lines additionally carry
//! `"oracle":[...]` (per-candidate average rollout reward from
//! `WarCampaign::oracle_values`, 3 seeds) and `"label"` (the argmax
//! candidate index). The episode length is unknown before play, so the
//! subsample is deterministic but approximate: a decision at ordinal `o`
//! is labeled when `o * K / 120` first exceeds the labels already emitted
//! (measured episodes run ~100-150 decisions), and labeling stops after K
//! labels.

use foch_eu4_decide::territory::{ProvincePool, Side, TerritoryMap, WarCampaign};
use foch_eu4_decide::world::Rng;
use std::io::{BufRead, Write};

/// Seeds per candidate in `WarCampaign::oracle_values`. Each seed clones
/// the campaign and rolls it to the horizon, so this multiplies the
/// per-label cost by candidates (<= 8) x seeds.
const ORACLE_SEEDS: u32 = 3;
/// Expected decisions per episode; drives the label spread (see header).
/// Measured real-map episodes run ~100-150 decisions.
const EXPECTED_DECISIONS: u64 = 120;

fn floats(values: &[f32]) -> String {
	let mut out = String::from("[");
	for (index, value) in values.iter().enumerate() {
		if index > 0 {
			out.push(',');
		}
		out.push_str(&format!("{value:.6}"));
	}
	out.push(']');
	out
}

fn main() {
	let args: Vec<String> = std::env::args().collect();
	let val = |name: &str, default: u64| -> u64 {
		args.iter()
			.position(|a| a == name)
			.and_then(|i| args.get(i + 1))
			.and_then(|raw| raw.parse().ok())
			.unwrap_or(default)
	};
	let scenarios = val("--scenarios", 64);
	let rollouts = val("--rollouts", 8);
	let oracle_labels = val("--oracle-labels", 0);
	let text_arg = |name: &str| -> Option<String> {
		args.iter()
			.position(|a| a == name)
			.and_then(|i| args.get(i + 1))
			.cloned()
	};
	let maps = text_arg("--maps").unwrap_or_else(|| "mixed".to_string());
	let personalities = [
		"glory_seeker_personality",
		"bold_fighter_personality",
		"careful_personality",
	];
	// The real province pool, if the maps call for it and the file is
	// anywhere findable.
	let mut pool: Option<ProvincePool> = None;
	if maps != "synth" {
		let mut candidates: Vec<String> = Vec::new();
		if let Some(path) = text_arg("--provinces") {
			candidates.push(path);
		}
		candidates.push("target/eu4_provinces.txt".to_string());
		candidates.push("../../../target/eu4_provinces.txt".to_string());
		for path in &candidates {
			if let Ok(contents) = std::fs::read_to_string(path) {
				pool = Some(ProvincePool::load(&contents));
				break;
			}
		}
		if pool.is_none() {
			eprintln!("territory_server: no province pool, falling back to synthetic maps");
		}
	}

	let stdin = std::io::stdin();
	let mut input = stdin.lock();
	let mut out = std::io::stdout().lock();

	for episode in 0..scenarios {
		let seed = 0x5eed_0000 + episode * 7919;
		let personality = personalities[(episode as usize) % personalities.len()];
		let use_real = match maps.as_str() {
			"real" => pool.is_some(),
			"synth" => false,
			_ => pool.is_some() && episode.is_multiple_of(2),
		};
		for rollout in 0..rollouts {
			let map = match (use_real, &pool) {
				(true, Some(data)) => {
					let mut map_rng = Rng::seed(seed ^ ((rollout + 1) * 31337));
					let size = 12 + map_rng.below(6) as usize;
					data.slice(&mut map_rng, size)
				}
				_ => TerritoryMap::generate(seed),
			};
			let mut war = WarCampaign::new_on_map(map, seed, personality);
			let mut rng = Rng::seed(seed ^ ((rollout + 1) * 104729));
			let mut line = String::new();
			// Per-episode oracle labeling state: the decision ordinal and
			// how many labels this episode has already emitted.
			let mut decision_ordinal: u64 = 0;
			let mut labeled_this_episode: u64 = 0;
			while !war.finished() {
				if war.decision_pending {
					decision_ordinal += 1;
					let candidates = war.candidates();
					let (w, h, cells) = war.board();
					let board: Vec<String> = cells.iter().map(|row| floats(row)).collect();
					let adj: Vec<String> = war
						.adjacency()
						.iter()
						.map(|e| format!("[{},{}]", e[0], e[1]))
						.collect();
					let coords: Vec<String> = candidates.iter().map(|c| format!("{}", c)).collect();
					// Deterministic ~K/episode subsample: label when the
					// ordinal's slot in an expected-120-decision episode
					// first moves past the labels already emitted.
					let mut oracle_json = String::new();
					if oracle_labels > 0
						&& labeled_this_episode < oracle_labels
						&& decision_ordinal * oracle_labels / EXPECTED_DECISIONS
							> labeled_this_episode
					{
						let values = war.oracle_values(&candidates, ORACLE_SEEDS);
						// Same argmax (last maximum wins) as oracle_label.
						let label = values
							.iter()
							.enumerate()
							.max_by(|a, b| {
								a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal)
							})
							.map(|(index, _)| index)
							.unwrap_or(0);
						oracle_json = format!(",\"oracle\":{},\"label\":{label}", floats(&values));
						labeled_this_episode += 1;
					}
					writeln!(
						out,
						"{{\"episode\":{episode},\"rollout\":{rollout},\"rules\":{},\"board\":{{\"w\":{},\"h\":{},\"cells\":[{}],\"adj\":[{}]}},\"candidates\":[{}],\"reward\":{:.6}{}}}",
						floats(&war.rules_vector()),
						w, h,
						board.join(","),
						adj.join(","),
						coords.join(","),
						war.reward(),
						oracle_json,
					).expect("stdout");
					out.flush().expect("flush");
					line.clear();
					if input.read_line(&mut line).unwrap_or(0) == 0 {
						return;
					}
					let pick: usize = line
						.find("\"pick\":")
						.and_then(|i| {
							line[i + 7..]
								.trim()
								.split(|c: char| !c.is_ascii_digit() && c != '-')
								.next()
								.and_then(|s| s.parse().ok())
						})
						.unwrap_or(0);
					let pick = pick.min(candidates.len() - 1);
					war.order(candidates[pick]);
				}
				war.step_day(&mut rng);
			}
			writeln!(
				out,
				"{{\"episode\":{episode},\"rollout\":{rollout},\"reward\":{:.6},\"done\":true,\"taken\":{},\"lost\":{},\"won\":{},\"lost_battles\":{},\"our_held\":{},\"their_held\":{},\"wipes\":{}}}",
				war.reward(),
				war.provinces_taken, war.provinces_lost,
				war.battles_won, war.battles_lost,
				war.map.held_by(Side::Ours).len(),
				war.map.held_by(Side::Theirs).len(),
				war.wipes,
			).expect("stdout");
			out.flush().expect("flush");
		}
	}
}
