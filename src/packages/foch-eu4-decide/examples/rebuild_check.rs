//! Wipe/rebuild dynamics under uniform play and a chasing hunter.
use foch_eu4_decide::territory::{Side, WarCampaign};
use foch_eu4_decide::world::Rng;

fn play(mode: &str) {
	let (mut wipes, mut rebuilds, mut taken, mut rewards) = (0, 0, 0u32, Vec::new());
	for ep in 0..32u64 {
		let seed = 0x5eed_0000 + ep * 7919;
		let mut war = WarCampaign::new(seed, "glory_seeker_personality");
		let mut rng = Rng::seed(seed ^ 104729);
		let mut alive = true;
		while !war.finished() {
			if war.decision_pending {
				let c = war.candidates();
				let pick = match mode {
					// Hunter: head for the nearest enemy stack, cheating on fog.
					"hunter" => *c
						.iter()
						.min_by_key(|&&t| {
							war.enemies
								.iter()
								.map(|stack| war.map.travel_days(t, stack.at, Side::Ours))
								.min()
								.unwrap_or(u32::MAX)
						})
						.unwrap(),
					_ => c[((seed + u64::from(war.day)) % c.len() as u64) as usize],
				};
				war.order(pick);
			}
			war.step_day(&mut rng);
			let now_alive = war
				.enemies
				.iter()
				.any(|stack| stack.army.regiments() >= 1.0);
			if alive && !now_alive {
				wipes += 1;
			}
			if !alive && now_alive {
				rebuilds += 1;
			}
			alive = now_alive;
		}
		taken += war.provinces_taken;
		rewards.push(war.reward());
	}
	rewards.sort_by(|a, b| a.partial_cmp(b).unwrap());
	println!(
		"{mode:8} wipes {wipes} rebuilds {rebuilds} | provinces taken {taken} | reward med {:+.1} max {:+.1}",
		rewards[16], rewards[31]
	);
}

fn main() {
	play("uniform");
	play("hunter");
}
