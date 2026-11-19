//! Host lifecycle and UI callbacks. A mixed submission clones the inactive
//! side's terms into the active offer for the submitted action and restores
//! the editor afterwards; the native evaluation of a merged offer is verified
//! only in game, so anything unexpected still refuses to submit.
use crate::{bridge, composition, native, profile, settlement, ui};
use core::ffi::c_void;
use foch_plugin_sdk::{self as abi, FochHostApi, FochPluginApi, FochStatus, FochStr};
use foch_runtime::dataplane::Memory;
use std::{
	panic::{AssertUnwindSafe, catch_unwind},
	sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering},
};

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static HOST: AtomicPtr<FochHostApi> = AtomicPtr::new(std::ptr::null_mut());
static BASE: AtomicUsize = AtomicUsize::new(0);
static STATE: AtomicI32 = AtomicI32::new(abi::state::INACTIVE);
static WARNED: AtomicBool = AtomicBool::new(false);

/// One submitted mixed offer awaiting settlement: a plugin-owned snapshot of
/// the give side's terms plus the submitted parties. The execution hook
/// consumes it to rebuild the settling selection. Confined to the game
/// thread, where submission and execution both run.
struct PendingGive {
	snapshot: native::OwnedOffer,
	giver: native::CountryRef,
	receiver: native::CountryRef,
}

static PENDING: AtomicPtr<PendingGive> = AtomicPtr::new(std::ptr::null_mut());

fn store_pending(pending: PendingGive) {
	let previous = PENDING.swap(Box::into_raw(Box::new(pending)), Ordering::AcqRel);
	if !previous.is_null() {
		unsafe { drop(Box::from_raw(previous)) };
	}
}

fn take_pending() -> Option<Box<PendingGive>> {
	let pointer = PENDING.swap(std::ptr::null_mut(), Ordering::AcqRel);
	(!pointer.is_null()).then(|| unsafe { Box::from_raw(pointer) })
}

static API: FochPluginApi = FochPluginApi {
	size: std::mem::size_of::<FochPluginApi>() as u32,
	abi_major: abi::ABI_MAJOR,
	abi_minor: abi::ABI_MINOR,
	caps: 0,
	init,
	status,
	config_changed: None,
	set_enabled: None,
	shutdown: None,
};

pub fn api(major: u32) -> *const FochPluginApi {
	if major == abi::ABI_MAJOR {
		&API
	} else {
		std::ptr::null()
	}
}

unsafe extern "C" fn init(host: *const FochHostApi) -> i32 {
	if host.is_null() || INITIALIZED.swap(true, Ordering::AcqRel) {
		return abi::result::E_INVALID;
	}
	HOST.store(host.cast_mut(), Ordering::Release);
	let host_ref = unsafe { &*host };
	if let Err(reason) = unsafe { profile::verify(host_ref) } {
		STATE.store(abi::state::REFUSED, Ordering::Release);
		log(abi::log::WARN, reason);
		return abi::result::OK;
	}
	BASE.store(host_ref.exe_base as usize, Ordering::Release);
	// Startup bisection switch: FOCH_PEACE_NO_REDIRECTS=1 installs no redirect
	// and reports active, so a failing game start can be split into "profile
	// verification" and "redirect behavior" halves.
	let noop = std::env::var_os("FOCH_PEACE_NO_REDIRECTS").is_some_and(|value| value == "1");
	let redirects: &[bridge::Redirect<'_>] = if noop {
		&[]
	} else {
		&[
			redirect(
				profile::SUMMARY_UPDATE,
				profile::SUMMARY.rva,
				summary as *const c_void,
			),
			redirect(
				profile::SUMMARY_SUGGEST,
				profile::SUMMARY.rva,
				summary as *const c_void,
			),
			redirect(
				profile::SUMMARY_SURRENDER,
				profile::SUMMARY.rva,
				summary as *const c_void,
			),
			redirect(
				profile::SEND_CALLBACK,
				profile::SEND.rva,
				send as *const c_void,
			),
			redirect(
				profile::EXECUTE_CALL,
				profile::EXECUTE.rva,
				execute as *const c_void,
			),
		]
	};
	match unsafe { bridge::install(host, redirects) } {
		Ok(()) => {
			STATE.store(abi::state::ACTIVE, Ordering::Release);
			log(
				abi::log::INFO,
				if noop {
					"Bilateral peace profile verified; redirects disabled by FOCH_PEACE_NO_REDIRECTS"
				} else {
					"Bilateral peace summaries installed; mixed submissions send a merged offer"
				},
			);
		}
		Err(error) => {
			STATE.store(abi::state::FAILED, Ordering::Release);
			log(abi::log::ERROR, &error.to_string());
		}
	}
	// The host retains the plugin even after a failed partial patch rollback.
	abi::result::OK
}

fn redirect(
	site: profile::CodeSite,
	target: usize,
	callback: *const c_void,
) -> bridge::Redirect<'static> {
	bridge::Redirect {
		site_rva: site.rva,
		expected_target_rva: target,
		original: site.original,
		callback,
	}
}

unsafe extern "C" fn status(out: *mut FochStatus) {
	if out.is_null() {
		return;
	}
	let state = STATE.load(Ordering::Acquire);
	let mut value = FochStatus::new(state);
	value.detail = FochStr::from_str(match state {
		abi::state::ACTIVE => {
			"Bilateral peace editor active; mixed submissions send a merged offer"
		}
		abi::state::REFUSED => "Unsupported game executable; original peace editor retained",
		abi::state::FAILED => "Peace editor patch installation failed; see plugin log",
		_ => "Peace editor has not initialized",
	});
	unsafe { out.write(value) };
}

fn engine() -> Result<native::Engine, native::NativeError> {
	unsafe {
		native::Engine::new(
			BASE.load(Ordering::Acquire),
			native::NativeRvas {
				offer_ctor: profile::OFFER_CONSTRUCTOR.rva,
				offer_copy_ctor: profile::OFFER_COPY.rva,
				offer_dtor: profile::OFFER_DESTROY.rva,
				treaty_clone: profile::CLONE_CLAUSE.rva,
				vector_emplace: profile::VECTOR_APPEND.rva,
				rebuild_cost: profile::REBUILD_COST.rva,
			},
		)
	}
}

unsafe extern "system" fn summary(view: *mut u8) {
	let base = BASE.load(Ordering::Acquire);
	let original: unsafe extern "system" fn(*mut u8) =
		unsafe { std::mem::transmute(base + profile::SUMMARY.rva) };
	unsafe { original(view) };
	if view.is_null() {
		return;
	}
	let result = catch_unwind(AssertUnwindSafe(|| unsafe {
		let engine = engine().map_err(|_| "cannot bind the peace editor")?;
		let receive = engine
			.read_draft(view.add(settlement::RECEIVE_OFFER))
			.map_err(|_| "cannot read demands")?;
		let give = engine
			.read_draft(view.add(settlement::GIVE_OFFER))
			.map_err(|_| "cannot read concessions")?;
		composition::compose(&receive, &give).map_err(|_| "unrelated peace drafts")?;
		// The previous refresh may have left the net value in the active
		// draft's cached total; rebuild both drafts so the scores read here
		// are the native one-direction values the net line is derived from.
		engine
			.rebuild_cost(view.add(settlement::RECEIVE_OFFER))
			.map_err(|_| "cannot rebuild demand costs")?;
		engine
			.rebuild_cost(view.add(settlement::GIVE_OFFER))
			.map_err(|_| "cannot rebuild concession costs")?;
		let receive_score = engine
			.score_total(view.add(settlement::RECEIVE_OFFER))
			.map_err(|_| "cannot read demand value")?;
		let give_score = engine
			.score_total(view.add(settlement::GIVE_OFFER))
			.map_err(|_| "cannot read concession value")?;
		// Once per process, dump the window's text-widget inventory so the
		// merged single-list agreement can target the widget that renders it.
		if !ui::windows::PROBED.swap(true, Ordering::AcqRel) {
			ui::windows::probe_text_widgets(base, view, |line| log(abi::log::INFO, line));
		}
		ui::windows::update_bilateral_summary(base, view, receive_score, give_score)
	}));
	let reason = match result {
		Ok(Ok(())) => return,
		Ok(Err(reason)) => reason,
		Err(_) => "panic while updating the bilateral peace preview",
	};
	if !WARNED.swap(true, Ordering::AcqRel) {
		log(abi::log::WARN, reason);
	}
}

unsafe extern "system" fn send(view: *mut u8) {
	let base = BASE.load(Ordering::Acquire);
	let original: unsafe extern "system" fn(*mut u8) =
		unsafe { std::mem::transmute(base + profile::SEND.rva) };
	if view.is_null() {
		return;
	}
	// The submission is prepared before the original send runs: the native
	// action copies the active offer, so the merge only has to be visible to
	// that copy. Restoring afterwards keeps the editor's two drafts intact.
	let prepared = catch_unwind(AssertUnwindSafe(|| unsafe {
		let engine = engine().map_err(|_| "cannot bind the peace editor")?;
		let direction = Memory
			.read_u8(
				(view as usize)
					.checked_add(ui::VIEW_DIRECTION)
					.ok_or("invalid peace view")?,
			)
			.ok_or("cannot read active peace direction")?;
		let active = if direction != 0 {
			composition::Side::Receive
		} else {
			composition::Side::Give
		};
		let receive = engine
			.read_draft(view.add(settlement::RECEIVE_OFFER))
			.map_err(|_| "cannot read demands")?;
		let give = engine
			.read_draft(view.add(settlement::GIVE_OFFER))
			.map_err(|_| "cannot read concessions")?;
		match settlement::plan(&receive, &give, active) {
			Err(settlement::PlanError::NothingToMerge) => {
				// A single-direction send settles only itself; any mixed
				// offer still awaiting settlement must not leak into it.
				take_pending();
				Ok(None)
			}
			Err(_) => Err("unrelated or invalid peace drafts"),
			Ok(plan) => {
				// Snapshot the give side into a plugin-owned offer before the
				// merge, so the execution hook can restore terms the response
				// pipeline drops even after the editor's drafts are gone.
				let snapshot = engine
					.copy_offer(view.add(settlement::GIVE_OFFER))
					.map_err(|_| "cannot snapshot the concessions for settlement")?;
				let parties = match plan.active {
					composition::Side::Receive => (receive.giver, receive.receiver),
					composition::Side::Give => (give.giver, give.receiver),
				};
				let active_ptr = view.add(settlement::offer_offset(plan.active));
				let merged = engine
					.append_selected_from(
						active_ptr,
						view.add(settlement::offer_offset(plan.active.opposite())),
					)
					.map_err(|_| "cannot merge the inactive peace terms")?;
				// Mirrored ducats between the same parties net into one plain
				// clause; the zeroed side is skipped by the native executor.
				let netted = engine
					.net_mirrored_ducats(active_ptr)
					.map_err(|_| "cannot net mirrored ducats")?;
				store_pending(PendingGive {
					snapshot,
					giver: parties.0,
					receiver: parties.1,
				});
				// The native rebuild priced every merged clause positively.
				// The give side must count against the deal: apply it once
				// negated to cancel what the rebuild added, and once more to
				// subtract its value, so the cached total becomes the net
				// deal that native validation and AI evaluation read. A
				// zeroed ducats contributes nothing on either pass.
				let give_side: &[composition::Clause] = if plan.active == composition::Side::Receive
				{
					&merged
				} else {
					&plan.restore
				};
				let direction = u32::from(
					native::offer_direction(active_ptr.cast_const())
						.map_err(|_| "cannot read the merged offer direction")?,
				);
				for clause in give_side {
					for negate in [true, true] {
						engine
							.apply_clause_cost(active_ptr, clause, direction, negate)
							.map_err(|_| "cannot reprice the merged peace terms")?;
					}
				}
				let net = engine
					.score_total(active_ptr)
					.map_err(|_| "cannot read the merged deal value")?;
				Ok(Some((plan, merged.len(), net, netted)))
			}
		}
	}));
	match prepared {
		Ok(Ok(None)) => unsafe { original(view) },
		Ok(Ok(Some((plan, merged, net, netted)))) if netted.is_empty() => {
			unsafe { original(view) };
			let restored = catch_unwind(AssertUnwindSafe(|| unsafe {
				let engine = engine().map_err(|_| ())?;
				engine
					.replace_selection(
						view.add(settlement::offer_offset(plan.active)),
						&plan.restore,
					)
					.map_err(|_| ())
			}));
			match restored {
				Ok(Ok(())) => {
					log(
						abi::log::INFO,
						&format!(
							"Mixed peace submission sent with {merged} merged terms, net deal {}, editor restored",
							ui::permille_percent(net)
						),
					);
					receipt(&[
						("action", "mixed_submit"),
						("net_deal", &ui::permille_percent(net)),
						("merged_terms", &merged.to_string()),
						("ducats_netted", "0"),
						("editor_restored", "yes"),
					]);
				}
				_ => {
					log(
						abi::log::WARN,
						"Mixed peace submission sent; the editor's prior selection was not restored",
					);
					receipt(&[
						("action", "mixed_submit"),
						("net_deal", &ui::permille_percent(net)),
						("merged_terms", &merged.to_string()),
						("ducats_netted", "0"),
						("editor_restored", "no"),
					]);
				}
			}
		}
		Ok(Ok(Some((plan, merged, net, netted)))) => {
			unsafe { original(view) };
			let restored = catch_unwind(AssertUnwindSafe(|| unsafe {
				let engine = engine().map_err(|_| ())?;
				engine.restore_amounts(&netted);
				engine
					.replace_selection(
						view.add(settlement::offer_offset(plan.active)),
						&plan.restore,
					)
					.map_err(|_| ())
			}));
			match restored {
				Ok(Ok(())) => {
					log(
						abi::log::INFO,
						&format!(
							"Mixed peace submission sent with {merged} merged terms ({} ducats edits netted), net deal {}, editor restored",
							netted.len(),
							ui::permille_percent(net)
						),
					);
					receipt(&[
						("action", "mixed_submit"),
						("net_deal", &ui::permille_percent(net)),
						("merged_terms", &merged.to_string()),
						("ducats_netted", &netted.len().to_string()),
						("editor_restored", "yes"),
					]);
				}
				_ => {
					log(
						abi::log::WARN,
						"Mixed peace submission sent; the editor's prior selection was not restored",
					);
					receipt(&[
						("action", "mixed_submit"),
						("net_deal", &ui::permille_percent(net)),
						("merged_terms", &merged.to_string()),
						("ducats_netted", &netted.len().to_string()),
						("editor_restored", "no"),
					]);
				}
			}
		}
		Ok(Err(reason)) => log(abi::log::ERROR, reason),
		Err(_) => log(
			abi::log::ERROR,
			"Peace draft preparation panicked; submission refused",
		),
	}
}

fn log(level: i32, message: &str) {
	let host = HOST.load(Ordering::Acquire);
	if !host.is_null() {
		unsafe { ((*host).log)(host, level, FochStr::from_str(message)) };
	}
}

/// Emits a structured `FOCH_RECEIPT/1` record for the test framework's
/// `expect_receipt` assertions. Every value is a string; no envelope key
/// (the host owns those) is repeated.
fn receipt(payload: &[(&str, &str)]) {
	let fields: Vec<String> = payload
		.iter()
		.map(|(key, value)| format!("{key:?}:{value:?}"))
		.collect();
	log(
		abi::log::INFO,
		&format!("FOCH_RECEIPT/1 {{{}}}", fields.join(",")),
	);
}

/// Runs every executed peace offer. If a mixed submission's give side was
/// dropped by the response pipeline, its snapshot's terms are injected back
/// into the settling offer first — with the same net repricing — so both
/// directions of the accepted treaty take effect. Afterwards the settled
/// offer is logged for verification.
/// The native executor resolves every clause's payee from the offer's
/// direction context, so a foreign-direction clause inside a merged offer
/// cannot settle correctly in one pass. Instead, each half is executed in
/// its own native direction: concessions first while the war is fully
/// alive, demands last, whose bookkeeping concludes the war like a native
/// one-direction treaty. If the second selection fails after the first half
/// settled, fall back to executing the combined selection once.
unsafe extern "system" fn execute(offer: *mut u8) {
	let base = BASE.load(Ordering::Acquire);
	let original: unsafe extern "system" fn(*mut u8) =
		unsafe { std::mem::transmute(base + profile::EXECUTE.rva) };
	if offer.is_null() {
		take_pending();
		unsafe { original(offer) };
		return;
	}
	if let Some(pending) = take_pending() {
		let restored = catch_unwind(AssertUnwindSafe(|| unsafe {
			let engine = engine().map_err(|_| ())?;
			let draft = engine.read_draft(offer.cast_const()).map_err(|_| ())?;
			if draft.giver != pending.giver || draft.receiver != pending.receiver {
				// A different peace settled; the pending offer is obsolete.
				return Ok((Vec::new(), Vec::new(), 0));
			}
			// The response pipeline rebuilds the settling offer and distorts
			// it: fabricated zero-amount ducats clauses, foreign amounts
			// zeroed. Rebuild the selection outright from what the player
			// actually submitted. The pipeline's own selection mixes both
			// directions, so the demand half keeps only clauses whose
			// parties match the offer's own direction; the concession half
			// comes entirely from the snapshot.
			let demands: Vec<composition::Clause> = draft
				.selected
				.iter()
				.filter(|clause| {
					native::clause_parties(clause.address).is_ok_and(|(giver, receiver)| {
						giver == draft.giver && receiver == draft.receiver
					})
				})
				.copied()
				.collect();
			engine.replace_selection(offer, &[]).map_err(|_| ())?;
			let snapshot = engine
				.read_draft(pending.snapshot.as_ptr())
				.map_err(|_| ())?;
			let concessions: Vec<composition::Clause> = snapshot.selected.clone();
			let appended = engine
				.append_selection_from(offer, pending.snapshot.as_ptr(), &concessions)
				.map_err(|_| ())?;
			let netted = engine.net_mirrored_ducats(offer).map_err(|_| ())?;
			Ok((demands, appended, netted.len()))
		}));
		match restored {
			Ok(Ok((demands, appended, _netted))) if demands.is_empty() && appended.is_empty() => unsafe {
				original(offer)
			},
			Ok(Ok((demands, appended, netted))) => {
				// One merged native pass while the war is alive: provinces,
				// releases and every other clause settle in their own
				// parties' terms. Only the give-side ducats cannot pay here
				// — the executor resolves payees from the offer's direction —
				// so they get a dedicated concessions-only pass afterwards.
				let Ok(engine) = engine() else {
					unsafe { original(offer) };
					return;
				};
				let combined: Vec<composition::Clause> =
					demands.iter().chain(appended.iter()).copied().collect();
				if unsafe { engine.replace_selection(offer, &combined) }.is_ok() {
					unsafe { original(offer) };
				} else {
					unsafe { original(offer) };
					log(
						abi::log::WARN,
						"The rebuilt selection could not be applied; settled the pipeline's offer as-is",
					);
				}
				// Give-side ducats that survived netting: a concessions-only
				// pass in the give direction, the one form proven to pay.
				let ducats: Vec<composition::Clause> = appended
					.iter()
					.filter(|clause| clause.kind == native::DUCATS_KIND)
					.copied()
					.collect();
				if !ducats.is_empty()
					&& ducats.iter().any(|clause| {
						native::amount_of(clause.address).is_some_and(|amount| amount != 0)
					}) {
					if let Ok(original_direction) = native::offer_direction(offer.cast_const())
						&& unsafe { engine.replace_selection(offer, &ducats) }.is_ok()
						&& unsafe { native::set_offer_direction(offer, 0) }.is_ok()
					{
						unsafe {
							original(offer);
							let _ = native::set_offer_direction(offer, original_direction);
						}
						log(
							abi::log::INFO,
							"Settled the give-side ducats in a dedicated concessions pass",
						);
					} else {
						log(
							abi::log::WARN,
							"Give-side ducats could not be settled; their amount is unpaid",
						);
					}
				}
				log(
					abi::log::INFO,
					&format!(
						"Merged-settled the mixed treaty: {} concessions, {} demands, {netted} ducats edits netted",
						appended.len(),
						demands.len()
					),
				);
				receipt(&[
					("action", "mixed_execute"),
					("concessions", &appended.len().to_string()),
					("demands", &demands.len().to_string()),
					("ducats_netted", &netted.to_string()),
				]);
			}
			Ok(Err(())) => {
				unsafe { original(offer) };
				log(
					abi::log::WARN,
					"Could not restore the give side of the mixed settlement",
				);
			}
			Err(_) => {
				unsafe { original(offer) };
				log(
					abi::log::WARN,
					"Panic while restoring the give side of the mixed settlement",
				);
			}
		}
	}
	let observed = catch_unwind(AssertUnwindSafe(|| unsafe {
		let engine = engine().map_err(|_| ())?;
		let draft = engine.read_draft(offer.cast_const()).map_err(|_| ())?;
		let direction = native::offer_direction(offer.cast_const()).map_err(|_| ())?;
		let score = engine.score_total(offer.cast_const()).map_err(|_| ())?;
		// Each clause's own parties decide who gives and who receives; log
		// them so a clause settling the wrong way — or not at all — is
		// attributable to a specific treaty, not just the offer.
		let terms: Vec<String> = draft
			.selected
			.iter()
			.map(|clause| {
				let (giver, receiver) = native::clause_parties(clause.address).unwrap_or((0, 0));
				// +0x30 carries the clause's amount; the ducats executor
				// silently skips a zero-amount clause.
				let amount = native::amount_of(clause.address).unwrap_or(0);
				format!(
					"kind {} target {:#x} amount {amount} giver {giver:#x} receiver {receiver:#x}",
					clause.kind, clause.target
				)
			})
			.collect();
		Ok::<_, ()>((draft.giver, draft.receiver, direction, score, terms))
	}));
	match observed {
		Ok(Ok((giver, receiver, direction, score, terms))) => log(
			abi::log::INFO,
			&format!(
				"executed peace offer: {} clauses, giver {giver:#x}, receiver {receiver:#x}, direction {direction}, cached score {}; [{}]",
				terms.len(),
				ui::permille_percent(score),
				terms.join("; ")
			),
		),
		_ => log(
			abi::log::INFO,
			"executed peace offer outside the verified editor layout",
		),
	}
}
