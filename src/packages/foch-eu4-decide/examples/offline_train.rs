//! One offline training run over sampled armies, drawn as a curve.
//!
//! `cargo run -p foch-eu4-decide --example offline_train`
//! `cargo run -p foch-eu4-decide --example offline_train -- --export DIR`
//!
//! Each checkpoint continues the deterministic episode stream instead of
//! replaying it, so the accuracy line is a real learning trajectory from the
//! untrained policy upward. The environment is the three-month pocket war:
//! a hidden enemy army, dice, interception risk on the roads, and the
//! engine-true score scales. `paid_order` is the label - the order the
//! environment itself pays most for that army - and it varies with the
//! hidden scenario, so the ceiling sits below a full score. With
//! `--export DIR`, the exact same sampled stream is written as
//! `groups.jsonl` and `eval.jsonl` for the offline torch trainer.
//!
//! Nothing here starts EU4: the booking environment stands in for
//! `AddEffectiveWarScore`.

use foch_eu4_decide::{
	ActionCost, GeneralPolicy, LoadedDatabase, NameTable, PlayerArmy, SampledArmy, Scenario,
	TrainingPool, WARGOAL_ENTRY_NAME, WarGoal, WarGoalKind, WarScoreLog, army_situation, book,
	draw_for, encode, entry_offset, order_list, paid_order, player_general_order, read_entry_name,
	warscore_per_cost,
};
use std::fmt::Write as _;
use std::path::PathBuf;

const IDEAS: &[&str] = &[
	"offensive_ideas",
	"defensive_ideas",
	"economic_ideas",
	"trade_ideas",
	"quality_ideas",
];
const LEADERS: &[&str] = &[
	"glory_seeker_personality",
	"bold_fighter_personality",
	"careful_personality",
];
const REFORMS: &[&str] = &["merchant_republic", "steppe_horde", "war_horde"];
/// The training pool repeats the battles-paid goal against the occupation
/// goals, so no order enjoys a stream-wide tilt.
const POOL_WARGOALS: &[&str] = &[
	"superiority",
	"superiority",
	"superiority",
	"take_province",
	"take_capital",
	"take_core",
	"take_border",
	"take_region",
];
const EVAL_WARGOALS: &[&str] = &[
	"superiority",
	"take_province",
	"take_capital",
	"take_core",
	"take_border",
	"take_region",
];
/// Scenario ids the training stream never draws; each is its own hidden war.
const EVAL_IDS: [i32; 8] = [901, 909, 917, 925, 933, 941, 949, 957];
const EXPORT_EPISODES: u32 = 1024;

/// Held-out armies: every wargoal under every personality, in scenario ids
/// the stream never reaches.
fn held_out() -> Vec<(PlayerArmy<'static>, &'static str, i32)> {
	let mut out = Vec::new();
	for personality in LEADERS {
		for wargoal in EVAL_WARGOALS {
			for id in EVAL_IDS {
				out.push((
					PlayerArmy {
						player: true,
						army: id,
						personality,
						ideas: &["offensive_ideas"],
						attributes: &["merchant_republic"],
						war_goal: WarGoal {
							kind: WarGoalKind::Superiority,
							province: None,
						},
						wargoal_name: wargoal,
					},
					*wargoal,
					id,
				));
			}
		}
	}
	out
}

fn sampled(id: i32, wargoal: &str, personality: &str) -> SampledArmy {
	SampledArmy {
		army: id,
		personality: personality.to_string(),
		ideas: vec!["offensive_ideas".to_string()],
		reform: String::new(),
		wargoal: wargoal.to_string(),
	}
}

fn rewards_of(army: &SampledArmy) -> [f32; 5] {
	let mut rewards = [0.0f32; 5];
	for (slot, order) in order_list().into_iter().enumerate() {
		let score = book(army, order);
		let mut log = WarScoreLog::default();
		log.add_effective(score.delta);
		rewards[slot] = warscore_per_cost(ActionCost::from_hook(
			&log,
			score.manpower_lost,
			score.gold_spent,
		));
	}
	rewards
}

/// Correct picks against `paid_order`, and one margin per wargoal class.
fn evaluate(policy: &GeneralPolicy, table: &NameTable) -> (usize, usize, f32, f32) {
	let armies = held_out();
	let mut right = 0;
	let mut decided = 0;
	for (army, wargoal, id) in &armies {
		if let Some(instruction) = player_general_order(policy, table, army) {
			decided += 1;
			if instruction.order == paid_order(&sampled(*id, wargoal, army.personality)) {
				right += 1;
			}
		}
	}
	let margin = |wargoal: &str| -> f32 {
		let mut sum = 0.0;
		for (army, _, _) in &armies {
			if army.wargoal_name != wargoal {
				continue;
			}
			let features = encode(&army_situation(table, army));
			let score = |row: usize| -> f32 {
				policy.rows[row]
					.iter()
					.zip(features)
					.map(|(weight, value)| weight * value)
					.sum()
			};
			sum += score(0) - score(1);
		}
		sum / LEADERS.len() as f32
	};
	(
		right,
		decided,
		margin("superiority"),
		margin("take_province"),
	)
}

fn bar(right: usize, total: usize) -> String {
	const WIDTH: usize = 24;
	let filled = right * WIDTH / total.max(1);
	format!("{}{}", "█".repeat(filled), "·".repeat(WIDTH - filled))
}

fn f32_list(values: &[f32]) -> String {
	let mut out = String::from("[");
	for (index, value) in values.iter().enumerate() {
		if index > 0 {
			out.push(',');
		}
		let _ = write!(out, "{value:.6}");
	}
	out.push(']');
	out
}

/// One name per pool object, read off the object bytes.
fn object_names(pool: &TrainingPool) -> (Vec<String>, Vec<String>) {
	let leaders = pool
		.objects
		.leaders
		.iter()
		.map(|object| {
			read_entry_name(
				object,
				entry_offset(LoadedDatabase::LeaderPersonalities).unwrap(),
				|pointer, size| pool.heap.read(pointer, size),
			)
			.unwrap_or_default()
		})
		.collect();
	let wargoals = pool
		.objects
		.wargoals
		.iter()
		.map(|object| {
			read_entry_name(object, WARGOAL_ENTRY_NAME, |pointer, size| {
				pool.heap.read(pointer, size)
			})
			.unwrap_or_default()
		})
		.collect();
	(leaders, wargoals)
}

/// Write the exact episode stream the trainer samples, plus the held-out
/// set with the environment's own paid label.
fn export(pool: &TrainingPool, dir: &PathBuf) {
	let (leaders, wargoals) = object_names(pool);
	let mut groups = String::new();
	for episode in 0..EXPORT_EPISODES {
		let draw = draw_for(
			pool.objects.leaders.len(),
			pool.objects.ideas.len(),
			pool.objects.reforms.len(),
			pool.objects.wargoals.len(),
			episode,
		);
		let id = episode as i32 + 1;
		let personality = &leaders[draw.personality];
		let wargoal = &wargoals[draw.wargoal];
		let army = sampled(id, wargoal, personality);
		let view = PlayerArmy {
			player: true,
			army: id,
			personality: personality.as_str(),
			ideas: &["offensive_ideas"],
			attributes: &["merchant_republic"],
			war_goal: WarGoal {
				kind: WarGoalKind::Superiority,
				province: None,
			},
			wargoal_name: wargoal.as_str(),
		};
		let features = encode(&army_situation(&pool.table, &view));
		let rewards = rewards_of(&army);
		let _ = writeln!(
			groups,
			"{{\"features\":{},\"rewards\":{}}}",
			f32_list(&features),
			f32_list(&rewards)
		);
	}
	std::fs::create_dir_all(dir).expect("create export dir");
	std::fs::write(dir.join("groups.jsonl"), groups).expect("write groups");
	let mut eval = String::new();
	for (army, wargoal, id) in held_out() {
		let features = encode(&army_situation(&pool.table, &army));
		let rewards = rewards_of(&sampled(id, wargoal, army.personality));
		let _ = writeln!(
			eval,
			"{{\"features\":{},\"rewards\":{}}}",
			f32_list(&features),
			f32_list(&rewards)
		);
	}
	std::fs::write(dir.join("eval.jsonl"), eval).expect("write eval");
}

fn main() {
	let pool = TrainingPool::build(IDEAS, LEADERS, REFORMS, POOL_WARGOALS);
	if let Some(index) = std::env::args().position(|arg| arg == "--export") {
		let dir = PathBuf::from(
			std::env::args()
				.nth(index + 1)
				.unwrap_or_else(|| "target/decide-episodes".to_string()),
		);
		export(&pool, &dir);
		println!("exported {EXPORT_EPISODES} episodes to {}", dir.display());
		return;
	}

	let mut policy = GeneralPolicy::zeros();
	let total = held_out().len();
	println!("held-out armies: {total}, paid order varies by hidden scenario");
	println!();
	println!("step        accuracy                     margin sup  margin take");
	let (right, decided, sup, take) = evaluate(&policy, &pool.table);
	println!(
		"{:>4}  {}  {:>2}/{} ({:>2} decided)  {:>+8.2}   {:>+8.2}",
		0,
		bar(right, total),
		right,
		total,
		decided,
		sup,
		take,
	);
	let mut rate = 0.2f32;
	for chunk in 1..=32u32 {
		rate *= 0.9;
		pool.train_chunk(&mut policy, (chunk - 1) * 8, 8, rate);
		let (right, decided, sup, take) = evaluate(&policy, &pool.table);
		println!(
			"{:>4}  {}  {:>2}/{} ({:>2} decided)  {:>+8.2}   {:>+8.2}",
			chunk * 8,
			bar(right, total),
			right,
			total,
			decided,
			sup,
			take,
		);
	}
	for id in EVAL_IDS {
		let scenario = Scenario::of(id);
		println!(
			"scenario {id}: enemy {:.0} regiments {}, relief month {}, {} provinces",
			scenario.enemy_regiments,
			if scenario.enemy_at_province {
				"on the goal"
			} else {
				"in the field"
			},
			scenario.relief_month,
			scenario.provinces,
		);
	}
}
