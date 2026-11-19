//! The pocket war as an environment server for the offline trainer.
//!
//! `cargo run -p foch-eu4-decide --example env_server -- --scenarios 64 --rollouts 8`
//!
//! The protocol is one JSON object per line over stdio. For each rollout
//! the server writes the world at every decision point -
//! `{"episode":n,"rollout":k,"global":[...],"candidates":[[...],...]}` -
//! and reads one `{"pick":i}` back. When the horizon passes it writes
//! `{"episode":n,"rollout":k,"reward":r,"done":true}` and moves on. The
//! scenario seed fixes the map, the armies, and the define draw; the
//! rollouts of one scenario share that world, which is exactly the group
//! the trainer's GRPO step needs.

use foch_eu4_decide::world::{Army, Campaign, Rng};
use std::io::{BufRead, Write};

fn main() {
	let args: Vec<String> = std::env::args().collect();
	let value = |name: &str, default: u64| -> u64 {
		args.iter()
			.position(|arg| arg == name)
			.and_then(|index| args.get(index + 1))
			.and_then(|raw| raw.parse().ok())
			.unwrap_or(default)
	};
	let scenarios = value("--scenarios", 64);
	let ally_weights = std::env::args()
		.position(|arg| arg == "--ally-policy")
		.and_then(|index| std::env::args().nth(index + 1))
		.and_then(|path| std::fs::read_to_string(path).ok())
		.and_then(|text| foch_eu4_decide::policy_net::PointerPolicy::from_json(&text));
	let enemy_weights = std::env::args()
		.position(|arg| arg == "--enemy-policy")
		.and_then(|index| std::env::args().nth(index + 1))
		.and_then(|path| std::fs::read_to_string(path).ok())
		.and_then(|text| foch_eu4_decide::policy_net::PointerPolicy::from_json(&text));
	let enemy_policy = enemy_weights.is_some();
	let oracle_labels = std::env::args().any(|arg| arg == "--oracle-labels");
	let armies_mode: String = std::env::args()
		.position(|arg| arg == "--armies")
		.and_then(|index| std::env::args().nth(index + 1))
		.unwrap_or_default();
	let two_armies = armies_mode == "2" || armies_mode == "both";
	let ally_army = armies_mode == "ally";
	let curriculum = std::env::args().any(|arg| arg == "--curriculum");
	let fixed_edge = std::env::args()
		.position(|arg| arg == "--edge")
		.and_then(|index| std::env::args().nth(index + 1))
		.map(|value| match value.as_str() {
			"ours" => foch_eu4_decide::world::Edge::Ours,
			"enemy" => foch_eu4_decide::world::Edge::Enemy,
			_ => foch_eu4_decide::world::Edge::Even,
		});
	let rollouts = value("--rollouts", 8);
	let personalities = [
		"glory_seeker_personality",
		"bold_fighter_personality",
		"careful_personality",
	];
	let wargoals = [
		"superiority",
		"take_province",
		"take_capital",
		"take_core",
		"take_border",
		"defend_capital",
	];

	let stdin = std::io::stdin();
	let mut input = stdin.lock();
	let mut out = std::io::stdout().lock();

	for episode in 0..scenarios {
		let seed = 0x5eed_0000 + episode * 7919;
		let personality = personalities[(episode as usize) % personalities.len()];
		let wargoal = wargoals[(episode as usize / personalities.len()) % wargoals.len()];
		for rollout in 0..rollouts {
			let edge = fixed_edge.unwrap_or(match episode % 3 {
				0 => foch_eu4_decide::world::Edge::Ours,
				1 => foch_eu4_decide::world::Edge::Even,
				_ => foch_eu4_decide::world::Edge::Enemy,
			});
			let edge = if curriculum || fixed_edge.is_some() {
				edge
			} else {
				foch_eu4_decide::world::Edge::Even
			};
			let mut campaign = Campaign::generate_with_edge(seed, personality, wargoal, edge);
			if two_armies {
				let second_pos = 1 + (seed % (campaign.map.provinces.len() as u64 - 1)) as u32;
				campaign.second_army_ai = ally_army && ally_weights.is_none();
				campaign.second_army = Some((
					second_pos,
					Army {
						infantry: 8.0,
						cavalry: 1.0,
						artillery: 1.0,
						morale: 1.0,
					},
				));
			}
			campaign.enemy_manual = enemy_policy;
			let mut rng = Rng::seed(seed ^ ((rollout + 1) * 104729));
			let mut line = String::new();
			while !campaign.finished() {
				if campaign.decision_pending {
					let candidates = campaign.candidates();
					let (width, height, cells) = campaign.board();
					let board: Vec<String> = cells.iter().map(|row| floats(row)).collect();
					let adj: Vec<String> = campaign
						.adjacency()
						.iter()
						.map(|e| format!("[{},{}]", e[0] as usize, e[1] as usize))
						.collect();
					let coords: Vec<String> = candidates
						.iter()
						.map(|index| {
							let province = &campaign.map.provinces[*index as usize];
							format!("[{},{},{}]", province.x + 1, province.y + 1, index)
						})
						.collect();
					let oracle = if oracle_labels {
						campaign.heuristic_label(&candidates, 4)
					} else {
						0
					};
					let description = campaign
						.describe(&candidates)
						.replace(char::from(10), "\\n")
						.replace(char::from(34), "\\\"");
					writeln!(
    out,
    "{{\"episode\":{episode},\"rollout\":{rollout},\"army\":{},\"rules\":{},\"board\":{{\"w\":{},\"h\":{},\"cells\":[{}],\"adj\":[{}]}},\"candidates\":[{}],\"oracle\":{},\"reward\":{},\"description\":\"{}\"}}",
    campaign.deciding,
    floats(&campaign.rules_vector()),
    width,
    height,
    board.join(","),
    adj.join(","),
    coords.join(","),
					oracle,
					campaign.reward(),
    description,
)
.expect("stdout");
					out.flush().expect("flush");
					line.clear();
					if campaign.deciding == 1
						&& let Some(weights) = &ally_weights
					{
						// Answer the ally's decision internally from the loaded
						// policy, without asking the main policy over stdio.
						let ally_candidates = campaign.candidates();
						let rules = campaign.rules_vector();
						let (width, height, cells) = campaign.board();
						let mut board = vec![0.0f32; width * height * 13];
						for cell in &cells {
							let x = cell[0] as usize;
							let y = cell[1] as usize;
							if x < width && y < height {
								let base = (y * width + x) * 13;
								board[base..base + cell.len()]
									.copy_from_slice(&cell[..13.min(cell.len())]);
								// The ally reads the board from its own perspective.
								board.swap(base + 9, base + 10);
								board.swap(base + 13, base + 9); // second army = "us" for ally
							}
						}
						let coords: Vec<(u32, u32)> = ally_candidates
							.iter()
							.map(|index| {
								let province = &campaign.map.provinces[*index as usize];
								(province.x + 1, province.y + 1)
							})
							.collect();
						let pick = weights.pick(&board, width, height, &rules, &coords);
						campaign.order_deciding(ally_candidates[pick]);
						continue;
					}
					if input.read_line(&mut line).unwrap_or(0) == 0 {
						return;
					}
					let pick: usize = parse_pick(&line).unwrap_or(0);
					let pick = pick.min(candidates.len() - 1);
					campaign.order_deciding(candidates[pick]);
				}
				if let Some(weights) = &enemy_weights {
					let candidates = campaign.candidates_from(campaign.enemy_position);
					let rules = campaign.rules_vector();
					let (width, height, cells) = campaign.board();
					let mut board = vec![0.0f32; width * height * 13];
					for cell in &cells {
						let x = cell[0] as usize;
						let y = cell[1] as usize;
						if x < width && y < height {
							let base = (y * width + x) * 13;
							board[base..base + cell.len()]
								.copy_from_slice(&cell[..13.min(cell.len())]);
							// The enemy reads the armies swapped.
							board.swap(base + 10, base + 11);
						}
					}
					let coords: Vec<(u32, u32)> = candidates
						.iter()
						.map(|index| {
							let province = &campaign.map.provinces[*index as usize];
							(province.x + 1, province.y + 1)
						})
						.collect();
					let pick = weights.pick(&board, width, height, &rules, &coords);
					campaign.enemy_order(candidates[pick]);
				}
				campaign.step_day(&mut rng);
			}
			writeln!(
				out,
				"{{\"episode\":{episode},\"rollout\":{rollout},\"reward\":{:.6},\"done\":true,\"exploration\":{:.3},\"occupation\":{:.3},\"ticking\":{:.3},\"battles\":{},\"won\":{},\"sieges\":{}}}",
				campaign.reward(),
				campaign.exploration_points,
				campaign.occupation_points,
				campaign.ticking_points,
				campaign.battle_count,
				campaign.battles_won,
				campaign.sieges_won
			)
			.expect("stdout");
			out.flush().expect("flush");
		}
	}
}

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

fn parse_pick(line: &str) -> Option<usize> {
	let start = line.find("\"pick\":")? + "\"pick\":".len();
	let rest = &line[start..];
	let digits: String = rest
		.trim_start()
		.chars()
		.take_while(char::is_ascii_digit)
		.collect();
	digits.parse().ok()
}
