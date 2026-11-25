//! The expert dataset generator for expert iteration.
//!
//! `cargo run -p foch-eu4-decide --example oracle_dataset -- --episodes 64`
//!
//! Runs episodes with the heuristic goal-seeking driver. At every decision
//! point the campaign rolls each candidate out to the horizon and labels
//! the slot with the best average reward; the state - the raw board, the
//! rules vector, and the candidate coordinates - is written as one JSONL
//! line for the behavior-cloning warm start.

use foch_eu4_decide::world::{Campaign, Rng};
use std::io::Write;

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

fn episodes_u32(total: u64) -> u32 {
	u32::try_from(total).unwrap_or(u32::MAX)
}

fn main() {
	let args: Vec<String> = std::env::args().collect();
	let policy_path = args
		.iter()
		.position(|arg| arg == "--policy")
		.and_then(|index| args.get(index + 1).map(std::path::PathBuf::from));
	let policy = policy_path.as_ref().and_then(|path| {
		let text = std::fs::read_to_string(path).expect("read policy");
		foch_eu4_decide::policy_net::PointerPolicy::from_json(&text)
	});
	if policy_path.is_some() && policy.is_none() {
		eprintln!("policy weights failed to parse; falling back to the heuristic");
	}
	let episodes: u64 = args
		.iter()
		.position(|arg| arg == "--episodes")
		.and_then(|index| args.get(index + 1))
		.and_then(|raw| raw.parse().ok())
		.unwrap_or(64);
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

	let mut out = std::io::stdout().lock();
	for episode in 0..episodes_u32(episodes) {
		let seed = 0x5eed_0000 + u64::from(episode) * 7919;
		let personality = personalities[(episode as usize) % personalities.len()];
		let wargoal = wargoals[(episode as usize / personalities.len()) % wargoals.len()];
		let mut campaign = Campaign::generate(seed, personality, wargoal);
		let mut rng = Rng::seed(seed ^ 104729);
		while !campaign.finished() {
			if campaign.decision_pending {
				let candidates = campaign.candidates();
				let best = match &policy {
					Some(weights) => campaign.lookahead_label(&candidates, 4, |branch, next| {
						let rules = branch.rules_vector();
						let (width, height, cells) = branch.board();
						let mut board = vec![0.0f32; width * height * 13];
						for cell in &cells {
							let x = cell[0] as usize;
							let y = cell[1] as usize;
							if x < width && y < height {
								let base = (y * width + x) * 13;
								board[base..base + cell.len()]
									.copy_from_slice(&cell[..13.min(cell.len())]);
							}
						}
						let coords: Vec<(u32, u32)> = next
							.iter()
							.map(|index| {
								let province = &branch.map.provinces[*index as usize];
								(province.x + 1, province.y + 1)
							})
							.collect();
						weights.pick(&board, width, height, &rules, &coords)
					}),
					None => campaign.heuristic_label(&candidates, 4),
				};
				let (width, height, cells) = campaign.board();
				let board: Vec<String> = cells.iter().map(|row| floats(row)).collect();
				let coords: Vec<String> = candidates
					.iter()
					.map(|index| {
						let province = &campaign.map.provinces[*index as usize];
						format!("[{},{},{}]", province.x + 1, province.y + 1, index)
					})
					.collect();
				let description = campaign
					.describe(&candidates)
					.replace(char::from(10), "\\n")
					.replace(char::from(34), "\\\"");
				writeln!(
					out,
					"{{\"episode\":{episode},\"rules\":{},\"board\":{{\"w\":{},\"h\":{},\"cells\":[{}]}},\"candidates\":[{}],\"best\":{best},\"description\":\"{}\"}}",
					floats(&campaign.rules_vector()),
					width,
					height,
					board.join(","),
					coords.join(","),
					description
				)
				.expect("stdout");
				out.flush().expect("flush");
				campaign.order(candidates[best]);
			}
			campaign.step_day(&mut rng);
		}
	}
}
