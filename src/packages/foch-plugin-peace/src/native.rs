//! Native EU4 offer ownership and selected-clause access.
//!
//! This module does not discover objects or enter the game process. Its unsafe
//! callers must provide live objects, the verified ABI, and the owning game
//! thread. Bounds checks reject malformed layouts; they cannot establish that an
//! arbitrary address is mapped or that a native virtual method is trustworthy.

use std::collections::BTreeSet;
use std::fmt;
use std::marker::PhantomData;
use std::ptr;

use foch_runtime::dataplane::{Memory, StdVector};

use crate::composition::{Clause, Draft};

pub type CountryRef = u64;
pub const OFFER_SIZE: usize = 0x1c0;
const OWNED_OFFSET: usize = 0x88;
const TABLE_OFFSET: usize = 0xd0;
const SELECTED_OFFSET: usize = 0x20;
/// Cached evaluation totals, rebuilt by the native cost rebuild; the signed
/// headline warscore value (per-mille) is the dword at `TOTALS_OFFSET + 8`.
const TOTALS_OFFSET: usize = 0xe8;
const SCORE_IN_TOTALS: usize = 8;
/// Direct offset of the cached headline warscore dword inside an offer.
pub const SCORE_TOTAL_OFFSET: usize = TOTALS_OFFSET + SCORE_IN_TOTALS;
/// The offer's direction byte: non-zero prices and submits as demands.
pub const OFFER_DIRECTION_OFFSET: usize = 0x20;
pub const GIVER_OFFSET: usize = 0x10;
pub const RECEIVER_OFFSET: usize = 0x18;
/// The clause's amount field; the ducats executor no-ops when it is zero.
pub const AMOUNT_OFFSET: usize = 0x30;
/// The ducats clause kind: mirrored ducats on both sides net into one.
pub const DUCATS_KIND: u32 = 0;
const KEY_VTABLE_OFFSET: usize = 0x78;
/// Signed clause-cost accumulator: (treaty, totals, direction, negate) adds
/// one clause's magnitude into the totals, negated when the last byte is set.
const ADD_COST_VTABLE_OFFSET: usize = 0x118;
const KIND_COUNT: usize = 40;
const MAX_CLAUSES: usize = 16_384;
const POINTER_SIZE: usize = 8;
const VECTOR_SIZE: usize = 24;

/// Function locations supplied by the already verified build profile.
#[derive(Clone, Copy, Debug)]
pub struct NativeRvas {
	pub offer_ctor: usize,
	pub offer_copy_ctor: usize,
	pub offer_dtor: usize,
	pub treaty_clone: usize,
	pub vector_emplace: usize,
	pub rebuild_cost: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeError {
	NullPointer,
	MisalignedPointer,
	AddressOverflow,
	MemoryAccess,
	InvalidVector,
	TooManyClauses,
	InvalidTableLength,
	NullClause,
	DuplicateClause,
	ClauseNotOwned,
	InvalidSelectionFlag,
	SelectionNotIndexed,
	IndexedClauseNotSelected,
	InvalidKind,
	KindMismatch,
	ClauseIdentityMismatch,
	InvalidKeyResult,
	CloneFailed,
	CloneAliased,
	AliasedOffers,
	NativeVectorMutation,
}

impl fmt::Display for NativeError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(formatter, "{self:?}")
	}
}

impl std::error::Error for NativeError {}

/// A native MSVC `std::vector` header, shared with `foch-runtime::dataplane`:
/// the same bounds, alignment and stride-divisibility rules, with the
/// element-count limit mapped onto this module's error variants.
#[derive(Clone, Copy)]
struct NativeVector {
	header: StdVector,
}

impl Default for NativeVector {
	fn default() -> Self {
		Self {
			header: StdVector {
				begin: 0,
				end: 0,
				capacity: 0,
			},
		}
	}
}

impl NativeVector {
	fn checked_len(self, stride: usize, limit: usize) -> Result<usize, NativeError> {
		self.header
			.len(stride, limit)
			.ok_or(NativeError::InvalidVector)
	}
}

#[derive(Default)]
#[repr(C)]
struct NativeKey {
	kind: u32,
	padding: u32,
	target: u64,
}

type OfferCtor = unsafe extern "system" fn(*mut u8, CountryRef, CountryRef, bool) -> *mut u8;
type OfferCopyCtor = unsafe extern "system" fn(*mut u8, *const u8) -> *mut u8;
type OfferDtor = unsafe extern "system" fn(*mut u8);
type TreatyClone = unsafe extern "system" fn(*const u8) -> *mut u8;
type VectorEmplace = unsafe extern "system" fn(*mut u8, *mut u8, *const *mut u8) -> *mut u8;
type RebuildCost = unsafe extern "system" fn(*mut u8);
type TreatyKey = unsafe extern "system" fn(*const u8, *mut NativeKey) -> *mut NativeKey;
type AddClauseCost = unsafe extern "system" fn(*const u8, *mut u8, u32, u8);

#[derive(Clone, Copy)]
struct NativeFunctions {
	ctor: OfferCtor,
	copy_ctor: OfferCopyCtor,
	dtor: OfferDtor,
	clone_treaty: TreatyClone,
	emplace: VectorEmplace,
	rebuild: RebuildCost,
}

/// A binding to one verified module, confined to its calling game thread.
pub struct Engine {
	base: usize,
	functions: NativeFunctions,
	_not_send: PhantomData<*mut ()>,
}

#[repr(C, align(8))]
struct OfferStorage([u8; OFFER_SIZE]);

/// Rust owns the outer allocation; the native non-deleting destructor owns all
/// of its members. No Rust allocator ever frees a native treaty or member vector.
pub struct OwnedOffer {
	storage: Box<OfferStorage>,
	destructor: OfferDtor,
	_not_send: PhantomData<*mut ()>,
}

impl OwnedOffer {
	pub fn as_ptr(&self) -> *const u8 {
		self.storage.0.as_ptr()
	}

	pub fn as_mut_ptr(&mut self) -> *mut u8 {
		self.storage.0.as_mut_ptr()
	}
}

impl Drop for OwnedOffer {
	fn drop(&mut self) {
		// SAFETY: construction initialized this storage with the paired native
		// constructor. Its allocator remains Rust's and is freed by Box afterward.
		unsafe { (self.destructor)(self.storage.0.as_mut_ptr()) };
	}
}

struct Snapshot {
	owned: Vec<usize>,
	selected: Vec<Clause>,
	buckets: [NativeVector; KIND_COUNT],
}

impl Engine {
	/// # Safety
	/// `base + rva` must resolve to the verified functions with these exact Win64
	/// signatures. The module and native runtime must outlive this engine and
	/// every offer it creates. Use only on the game's owning thread.
	pub unsafe fn new(base: usize, rvas: NativeRvas) -> Result<Self, NativeError> {
		fn address(base: usize, rva: usize) -> Result<usize, NativeError> {
			if base == 0 || rva == 0 {
				return Err(NativeError::NullPointer);
			}
			base.checked_add(rva).ok_or(NativeError::AddressOverflow)
		}
		let functions = unsafe {
			NativeFunctions {
				ctor: std::mem::transmute::<usize, OfferCtor>(address(base, rvas.offer_ctor)?),
				copy_ctor: std::mem::transmute::<usize, OfferCopyCtor>(address(
					base,
					rvas.offer_copy_ctor,
				)?),
				dtor: std::mem::transmute::<usize, OfferDtor>(address(base, rvas.offer_dtor)?),
				clone_treaty: std::mem::transmute::<usize, TreatyClone>(address(
					base,
					rvas.treaty_clone,
				)?),
				emplace: std::mem::transmute::<usize, VectorEmplace>(address(
					base,
					rvas.vector_emplace,
				)?),
				rebuild: std::mem::transmute::<usize, RebuildCost>(address(
					base,
					rvas.rebuild_cost,
				)?),
			}
		};
		Ok(Self {
			base,
			functions,
			_not_send: PhantomData,
		})
	}

	pub fn module_base(&self) -> usize {
		self.base
	}

	/// # Safety
	/// Country references and `direction` must be valid for this native game
	/// state. The runtime and module must remain live until the returned offer
	/// is dropped on this same thread.
	pub unsafe fn new_offer(
		&self,
		giver: CountryRef,
		receiver: CountryRef,
		direction: bool,
	) -> Result<OwnedOffer, NativeError> {
		let mut storage = Box::new(OfferStorage([0; OFFER_SIZE]));
		unsafe { (self.functions.ctor)(storage.0.as_mut_ptr(), receiver, giver, direction) };
		let offer = OwnedOffer {
			storage,
			destructor: self.functions.dtor,
			_not_send: PhantomData,
		};
		unsafe { self.snapshot(offer.as_ptr())? };
		Ok(offer)
	}

	/// # Safety
	/// `source` must be a live, readable offer for this engine. Its entire native
	/// ownership graph and virtual functions must be valid. The returned offer
	/// has the same runtime and thread lifetime requirements as `new_offer`.
	pub unsafe fn copy_offer(&self, source: *const u8) -> Result<OwnedOffer, NativeError> {
		unsafe { self.snapshot(source)? };
		let mut storage = Box::new(OfferStorage([0; OFFER_SIZE]));
		unsafe { (self.functions.copy_ctor)(storage.0.as_mut_ptr(), source) };
		let offer = OwnedOffer {
			storage,
			destructor: self.functions.dtor,
			_not_send: PhantomData,
		};
		unsafe { self.snapshot(offer.as_ptr())? };
		Ok(offer)
	}

	/// # Safety
	/// The complete native offer graph and its virtual methods must be live and
	/// readable for the duration of this call, without concurrent mutation.
	pub unsafe fn read_draft(&self, offer: *const u8) -> Result<Draft, NativeError> {
		let snapshot = unsafe { self.snapshot(offer)? };
		let offer_addr = offer as usize;
		Ok(Draft {
			giver: read_u64(add_offset(offer_addr, GIVER_OFFSET)?)?,
			receiver: read_u64(add_offset(offer_addr, RECEIVER_OFFSET)?)?,
			selected: snapshot.selected,
		})
	}

	/// Replaces selected terms using identities already owned by this offer.
	/// Validation completes before flags or table entries change. Omitted
	/// treaties remain in the owned vector for native destruction.
	///
	/// # Safety
	/// `offer` must be a live, exclusively mutable native offer for this engine.
	/// Its complete graph and virtual methods must satisfy `read_draft`'s contract.
	pub unsafe fn replace_selection(
		&self,
		offer: *mut u8,
		clauses: &[Clause],
	) -> Result<(), NativeError> {
		let snapshot = unsafe { self.snapshot(offer)? };
		let owned: BTreeSet<_> = snapshot.owned.iter().copied().collect();
		let mut seen = BTreeSet::new();
		for clause in clauses {
			if !owned.contains(&clause.address) {
				return Err(NativeError::ClauseNotOwned);
			}
			if !seen.insert(clause.address) {
				return Err(NativeError::DuplicateClause);
			}
			let actual = unsafe { self.clause(clause.address as *const u8)? };
			if actual != *clause {
				return Err(NativeError::ClauseIdentityMismatch);
			}
		}
		for &address in &snapshot.owned {
			unsafe { write_u8(add_offset(address, SELECTED_OFFSET)?, 0)? };
		}
		let table = read_vector(offer.cast_const(), TABLE_OFFSET)?;
		for (kind, bucket) in snapshot.buckets.iter().enumerate() {
			let header = add_offset(table.header.begin, kind * VECTOR_SIZE)?;
			unsafe {
				write_vector_at(
					header,
					NativeVector {
						header: StdVector {
							end: bucket.header.begin,
							..bucket.header
						},
					},
				)?
			};
		}
		for clause in clauses {
			let header =
				add_offset(table.header.begin, clause.kind as usize * VECTOR_SIZE)? as *mut u8;
			unsafe { self.push_pointer(header, clause.address as *mut u8)? };
			unsafe { write_u8(add_offset(clause.address, SELECTED_OFFSET)?, 1)? };
		}
		unsafe { (self.functions.rebuild)(offer) };
		Ok(())
	}

	/// Filters by explicit clause identity; no treaty party offsets are used to
	/// infer direction. The caller supplies its already classified address set.
	///
	/// # Safety
	/// The exclusive native-object contract of `replace_selection` applies.
	pub unsafe fn retain_selected(
		&self,
		offer: *mut u8,
		mut keep: impl FnMut(&Clause) -> bool,
	) -> Result<(), NativeError> {
		let selected = unsafe { self.read_draft(offer)? }.selected;
		let retained: Vec<_> = selected.into_iter().filter(|clause| keep(clause)).collect();
		unsafe { self.replace_selection(offer, &retained) }
	}

	/// Reads the offer's cached headline warscore value (per-mille units).
	///
	/// # Safety
	/// `offer` must be a live, readable offer for this engine.
	pub unsafe fn score_total(&self, offer: *const u8) -> Result<i32, NativeError> {
		check_pointer(offer)?;
		read_i32(add_offset(offer as usize, SCORE_TOTAL_OFFSET)?)
	}

	/// Overwrites the offer's cached headline warscore value. The summary hook
	/// uses it to keep the native cost display on the merged deal's net value
	/// while both drafts have terms; the native rebuild restores the
	/// one-direction value on its next pass.
	///
	/// # Safety
	/// `offer` must be a live, exclusively writable offer for this engine.
	pub unsafe fn set_score_total(&self, offer: *mut u8, value: i32) -> Result<(), NativeError> {
		check_pointer(offer)?;
		unsafe { write_i32(add_offset(offer as usize, SCORE_TOTAL_OFFSET)?, value) }
	}

	/// Forces the native cost rebuild of one offer, refreshing its cached
	/// one-direction total. The summary display overwrites the active draft's
	/// total with the net deal value, so reading it back must rebuild first.
	///
	/// # Safety
	/// The exclusive native-object contract of `replace_selection` applies.
	pub unsafe fn rebuild_cost(&self, offer: *mut u8) -> Result<(), NativeError> {
		unsafe { self.snapshot(offer)? };
		unsafe { (self.functions.rebuild)(offer) };
		Ok(())
	}

	/// Nets mirrored ducats clauses inside one merged offer: when both
	/// directions carry ducats between the same parties, the larger amount
	/// absorbs the smaller and the losing side is zeroed, which the native
	/// ducats executor skips. The submission therefore carries one plain
	/// net clause the native pipeline fully understands. Returned edits
	/// carry the originals for [`Engine::restore_amounts`].
	///
	/// # Safety
	/// The exclusive native-object contract of `replace_selection` applies.
	pub unsafe fn net_mirrored_ducats(
		&self,
		offer: *mut u8,
	) -> Result<Vec<AmountEdit>, NativeError> {
		let draft = unsafe { self.read_draft(offer)? };
		let mut edits = Vec::new();
		for (index, clause) in draft.selected.iter().enumerate() {
			if clause.kind != DUCATS_KIND {
				continue;
			}
			let amount = clause_amount(clause.address as *const u8)?;
			if amount == 0 {
				continue;
			}
			let giver = treaty_party(clause.address, GIVER_OFFSET)?;
			let receiver = treaty_party(clause.address, RECEIVER_OFFSET)?;
			// Find a later mirrored ducats clause between the same parties.
			let twin = draft.selected[index + 1..].iter().find(|other| {
				other.kind == DUCATS_KIND
					&& treaty_party(other.address, GIVER_OFFSET)
						.is_ok_and(|value| value == receiver)
					&& treaty_party(other.address, RECEIVER_OFFSET)
						.is_ok_and(|value| value == giver)
			});
			let Some(twin) = twin else { continue };
			let twin_amount = clause_amount(twin.address as *const u8)?;
			if twin_amount == 0 {
				continue;
			}
			let (keep, lose) = if amount >= twin_amount {
				(amount - twin_amount, 0)
			} else {
				(twin_amount - amount, 0)
			};
			let keep_edit = AmountEdit {
				address: if amount >= twin_amount {
					clause.address
				} else {
					twin.address
				},
				original: if amount >= twin_amount {
					amount
				} else {
					twin_amount
				},
				replacement: keep,
			};
			let lose_edit = AmountEdit {
				address: if amount >= twin_amount {
					twin.address
				} else {
					clause.address
				},
				original: if amount >= twin_amount {
					twin_amount
				} else {
					amount
				},
				replacement: lose,
			};
			for edit in [keep_edit, lose_edit] {
				unsafe { write_i32(add_offset(edit.address, AMOUNT_OFFSET)?, edit.replacement)? };
				edits.push(edit);
			}
		}
		Ok(edits)
	}

	/// Writes the originals back after the submission copied the merged
	/// offer, so the editor's own drafts keep the amounts the player set.
	///
	/// # Safety
	/// Every address must still belong to a live treaty of this engine.
	pub unsafe fn restore_amounts(&self, edits: &[AmountEdit]) {
		for edit in edits {
			if edit.original != edit.replacement
				&& let Ok(addr) = add_offset(edit.address, AMOUNT_OFFSET)
			{
				let _ = unsafe { write_i32(addr, edit.original) };
			}
		}
	}

	/// Restores zeroed clause amounts from a snapshot offer. The response
	/// pipeline rebuilds a settled offer with the foreign-direction ducats
	/// clause's amount zeroed, and the ducats executor silently skips a
	/// zero-amount clause. A clause matches its snapshot twin by kind, target
	/// and both parties; only nonzero snapshot amounts are copied. Directions
	/// listed in `preserve_zero` were netted to zero at submission and stay
	/// zero.
	///
	/// # Safety
	/// Both offers must be live for this engine on the owning thread; `offer`
	/// is exclusively mutable.
	pub unsafe fn repair_clause_amounts(
		&self,
		offer: *mut u8,
		snapshot: *const u8,
		preserve_zero: &[(u32, u64, u64)],
	) -> Result<usize, NativeError> {
		let executed = unsafe { self.read_draft(offer)? };
		let source = unsafe { self.read_draft(snapshot)? };
		let mut repaired = 0;
		for clause in &executed.selected {
			let amount = clause_amount(clause.address as *const u8)?;
			if amount != 0 {
				continue;
			}
			let giver = treaty_party(clause.address, GIVER_OFFSET)?;
			let receiver = treaty_party(clause.address, RECEIVER_OFFSET)?;
			if preserve_zero.contains(&(clause.kind, giver, receiver)) {
				continue;
			}
			for twin in &source.selected {
				if twin.kind != clause.kind
					|| twin.target != clause.target
					|| treaty_party(twin.address, GIVER_OFFSET)? != giver
					|| treaty_party(twin.address, RECEIVER_OFFSET)? != receiver
				{
					continue;
				}
				let replacement = clause_amount(twin.address as *const u8)?;
				if replacement != 0 {
					unsafe { write_i32(add_offset(clause.address, AMOUNT_OFFSET)?, replacement)? };
					repaired += 1;
					break;
				}
			}
		}
		Ok(repaired)
	}

	/// Applies one selected clause's cost into the cached totals with an
	/// explicit sign. The native rebuild adds every selected clause with a
	/// positive sign; re-applying the inactive side's clauses with `negate`
	/// turns a merged offer's cached total into the net deal value that the
	/// native display, warscore validation and AI evaluation all read.
	///
	/// # Safety
	/// The exclusive native-object contract of `replace_selection` applies.
	pub unsafe fn apply_clause_cost(
		&self,
		offer: *mut u8,
		clause: &Clause,
		direction: u32,
		negate: bool,
	) -> Result<(), NativeError> {
		let snapshot = unsafe { self.snapshot(offer)? };
		if !snapshot.selected.contains(clause) {
			return Err(NativeError::ClauseIdentityMismatch);
		}
		let treaty = clause.address as *const u8;
		let vtable = read_u64(clause.address)? as usize;
		let slot = add_offset(vtable, ADD_COST_VTABLE_OFFSET)?;
		let method = read_u64(slot)? as usize;
		if method == 0 {
			return Err(NativeError::NullPointer);
		}
		let add_cost = unsafe { std::mem::transmute::<usize, AddClauseCost>(method) };
		unsafe {
			add_cost(
				treaty,
				offer.add(TOTALS_OFFSET),
				direction,
				u8::from(negate),
			)
		};
		Ok(())
	}

	/// Deep-clones only the source's currently selected terms into `destination`.
	///
	/// # Safety
	/// Both graphs must be live for this engine, mutually disjoint, and exclusively
	/// accessed on the owning native thread. Destination is mutable. Native clone
	/// and vector allocation must obey their verified ownership contracts.
	pub unsafe fn append_selected_from(
		&self,
		destination: *mut u8,
		source: *const u8,
	) -> Result<Vec<Clause>, NativeError> {
		let clauses = unsafe { self.read_draft(source)? }.selected;
		unsafe { self.append_selection_from(destination, source, &clauses) }
	}

	/// Deep-clones an explicit subset of source selections. Returned identities
	/// refer to the new destination allocations. A native clone failure can leave
	/// earlier successful clones selected; all accepted allocations remain owned
	/// by the destination and its cost is rebuilt before returning.
	///
	/// # Safety
	/// The native graph and allocation contract of `append_selected_from` applies.
	pub unsafe fn append_selection_from(
		&self,
		destination: *mut u8,
		source: *const u8,
		clauses: &[Clause],
	) -> Result<Vec<Clause>, NativeError> {
		if destination.cast_const() == source {
			return Err(NativeError::AliasedOffers);
		}
		let source_snapshot = unsafe { self.snapshot(source)? };
		let destination_snapshot = unsafe { self.snapshot(destination)? };
		let mut forbidden: BTreeSet<_> = source_snapshot.owned.iter().copied().collect();
		for &address in &destination_snapshot.owned {
			if !forbidden.insert(address) {
				return Err(NativeError::AliasedOffers);
			}
		}
		if destination_snapshot
			.owned
			.len()
			.saturating_add(clauses.len())
			> MAX_CLAUSES
		{
			return Err(NativeError::TooManyClauses);
		}
		let mut seen = BTreeSet::new();
		for clause in clauses {
			if !seen.insert(clause.address) {
				return Err(NativeError::DuplicateClause);
			}
			if !source_snapshot.selected.contains(clause) {
				return Err(NativeError::ClauseIdentityMismatch);
			}
		}
		let table = read_vector(destination, TABLE_OFFSET)?;
		let result = (|| {
			let mut appended = Vec::with_capacity(clauses.len());
			for clause in clauses {
				let cloned = unsafe { (self.functions.clone_treaty)(clause.address as *const u8) };
				if cloned.is_null() {
					return Err(NativeError::CloneFailed);
				}
				if !forbidden.insert(cloned as usize) {
					return Err(NativeError::CloneAliased);
				}
				check_pointer(cloned)?;
				// Keep every accepted native allocation under exactly one native
				// destructor, including a clone whose returned key proves invalid.
				unsafe { write_u8(add_offset(cloned as usize, SELECTED_OFFSET)?, 0)? };
				unsafe { self.push_pointer(destination.add(OWNED_OFFSET), cloned)? };
				let identity = unsafe { self.clause(cloned)? };
				if identity.kind != clause.kind || identity.target != clause.target {
					return Err(NativeError::ClauseIdentityMismatch);
				}
				let header = add_offset(table.header.begin, identity.kind as usize * VECTOR_SIZE)?
					as *mut u8;
				unsafe { self.push_pointer(header, cloned)? };
				unsafe { write_u8(add_offset(cloned as usize, SELECTED_OFFSET)?, 1)? };
				appended.push(identity);
			}
			Ok(appended)
		})();
		unsafe { (self.functions.rebuild)(destination) };
		result
	}

	unsafe fn snapshot(&self, offer: *const u8) -> Result<Snapshot, NativeError> {
		check_pointer(offer)?;
		let owned = pointer_elements(read_vector(offer, OWNED_OFFSET)?)?;
		let mut owned_set = BTreeSet::new();
		for &address in &owned {
			if address == 0 {
				return Err(NativeError::NullClause);
			}
			check_pointer(address as *const u8)?;
			if !owned_set.insert(address) {
				return Err(NativeError::DuplicateClause);
			}
		}
		let table = read_vector(offer, TABLE_OFFSET)?;
		if table.checked_len(VECTOR_SIZE, KIND_COUNT)? != KIND_COUNT {
			return Err(NativeError::InvalidTableLength);
		}
		let mut buckets = [NativeVector::default(); KIND_COUNT];
		let mut indexed = BTreeSet::new();
		let mut selected = Vec::new();
		for (kind, bucket) in buckets.iter_mut().enumerate() {
			*bucket = read_vector_at(add_offset(table.header.begin, kind * VECTOR_SIZE)?)?;
			for address in pointer_elements(*bucket)? {
				if address == 0 {
					return Err(NativeError::NullClause);
				}
				if !owned_set.contains(&address) {
					return Err(NativeError::ClauseNotOwned);
				}
				if !indexed.insert(address) {
					return Err(NativeError::DuplicateClause);
				}
				if selection_flag(address)? != 1 {
					return Err(NativeError::IndexedClauseNotSelected);
				}
				let clause = unsafe { self.clause(address as *const u8)? };
				if clause.kind as usize != kind {
					return Err(NativeError::KindMismatch);
				}
				selected.push(clause);
			}
		}
		for &address in &owned {
			if selection_flag(address)? == 1 && !indexed.contains(&address) {
				return Err(NativeError::SelectionNotIndexed);
			}
		}
		Ok(Snapshot {
			owned,
			selected,
			buckets,
		})
	}

	unsafe fn clause(&self, treaty: *const u8) -> Result<Clause, NativeError> {
		check_pointer(treaty)?;
		let vtable = read_u64(treaty as usize)? as usize;
		check_pointer(vtable as *const u8)?;
		let slot = add_offset(vtable, KEY_VTABLE_OFFSET)?;
		let method = read_u64(slot)? as usize;
		if method == 0 {
			return Err(NativeError::NullPointer);
		}
		let getter = unsafe { std::mem::transmute::<usize, TreatyKey>(method) };
		let mut key = NativeKey::default();
		let returned = unsafe { getter(treaty, &mut key) };
		if !ptr::eq(returned, &key) {
			return Err(NativeError::InvalidKeyResult);
		}
		if key.kind as usize >= KIND_COUNT {
			return Err(NativeError::InvalidKind);
		}
		Ok(Clause {
			address: treaty as usize,
			kind: key.kind,
			target: key.target,
		})
	}

	unsafe fn push_pointer(&self, vector: *mut u8, value: *mut u8) -> Result<(), NativeError> {
		let vector_addr = vector as usize;
		let current = read_vector_at(vector_addr)?;
		let header = current.header;
		let previous_len = current.checked_len(POINTER_SIZE, MAX_CLAUSES)?;
		if previous_len == MAX_CLAUSES {
			return Err(NativeError::TooManyClauses);
		}
		if header.end == header.capacity {
			unsafe { (self.functions.emplace)(vector, header.end as *mut u8, &value) };
		} else {
			unsafe { write_u64(header.end, value as u64)? };
			let next_end = add_offset(header.end, POINTER_SIZE)?;
			unsafe {
				write_vector_at(
					vector_addr,
					NativeVector {
						header: StdVector {
							end: next_end,
							..header
						},
					},
				)?
			};
		}
		let updated = read_vector_at(vector_addr)?;
		if updated.checked_len(POINTER_SIZE, MAX_CLAUSES)? != previous_len + 1
			|| read_u64(updated.header.end - POINTER_SIZE)? as usize != value as usize
		{
			return Err(NativeError::NativeVectorMutation);
		}
		Ok(())
	}
}

fn add_offset(base: usize, offset: usize) -> Result<usize, NativeError> {
	base.checked_add(offset).ok_or(NativeError::AddressOverflow)
}

fn read_u8(addr: usize) -> Result<u8, NativeError> {
	Memory.read_u8(addr).ok_or(NativeError::MemoryAccess)
}

fn read_i32(addr: usize) -> Result<i32, NativeError> {
	Memory.read_i32(addr).ok_or(NativeError::MemoryAccess)
}

fn read_u64(addr: usize) -> Result<u64, NativeError> {
	Memory.read_qword(addr).ok_or(NativeError::MemoryAccess)
}

unsafe fn write_u8(addr: usize, value: u8) -> Result<(), NativeError> {
	unsafe { Memory.write_bytes(addr, &[value]) }.ok_or(NativeError::MemoryAccess)
}

unsafe fn write_i32(addr: usize, value: i32) -> Result<(), NativeError> {
	unsafe { Memory.write_i32(addr, value) }.ok_or(NativeError::MemoryAccess)
}

unsafe fn write_u64(addr: usize, value: u64) -> Result<(), NativeError> {
	unsafe { Memory.write_qword(addr, value) }.ok_or(NativeError::MemoryAccess)
}

fn clause_amount(treaty: *const u8) -> Result<i32, NativeError> {
	check_pointer(treaty)?;
	read_i32(add_offset(treaty as usize, AMOUNT_OFFSET)?)
}

/// Reads one clause's amount through the guarded reader; the ducats executor
/// skips a zero-amount clause.
pub fn amount_of(clause_address: usize) -> Option<i32> {
	let addr = clause_address.checked_add(AMOUNT_OFFSET)?;
	Memory.read_i32(addr)
}

/// Reads a clause's `(giver, receiver)` country references through the guarded reader.
pub fn clause_parties(clause_address: usize) -> Result<(CountryRef, CountryRef), NativeError> {
	Ok((
		treaty_party(clause_address, GIVER_OFFSET)?,
		treaty_party(clause_address, RECEIVER_OFFSET)?,
	))
}

/// Reads an offer's direction byte (`OFFER_DIRECTION_OFFSET`) through the guarded reader.
pub fn offer_direction(offer: *const u8) -> Result<u8, NativeError> {
	check_pointer(offer)?;
	read_u8(add_offset(offer as usize, OFFER_DIRECTION_OFFSET)?)
}

/// Overwrites an offer's direction byte (`OFFER_DIRECTION_OFFSET`) through the guarded writer.
///
/// # Safety
/// `offer` must be a live, exclusively writable native offer on the owning game thread.
pub unsafe fn set_offer_direction(offer: *mut u8, direction: u8) -> Result<(), NativeError> {
	check_pointer(offer)?;
	unsafe {
		write_u8(
			add_offset(offer as usize, OFFER_DIRECTION_OFFSET)?,
			direction,
		)
	}
}

/// One amount rewrite from netting mirrored ducats, restorable after the
/// submission copied the merged offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AmountEdit {
	pub address: usize,
	pub original: i32,
	pub replacement: i32,
}

fn treaty_party(treaty: usize, offset: usize) -> Result<u64, NativeError> {
	check_pointer(treaty as *const u8)?;
	read_u64(add_offset(treaty, offset)?)
}

fn check_pointer(pointer: *const u8) -> Result<(), NativeError> {
	if pointer.is_null() {
		Err(NativeError::NullPointer)
	} else if !(pointer as usize).is_multiple_of(POINTER_SIZE) {
		Err(NativeError::MisalignedPointer)
	} else {
		Ok(())
	}
}

fn read_vector_at(addr: usize) -> Result<NativeVector, NativeError> {
	StdVector::read(&Memory, addr)
		.map(|header| NativeVector { header })
		.ok_or(NativeError::InvalidVector)
}

fn read_vector(offer: *const u8, offset: usize) -> Result<NativeVector, NativeError> {
	read_vector_at(add_offset(offer as usize, offset)?)
}

unsafe fn write_vector_at(addr: usize, vector: NativeVector) -> Result<(), NativeError> {
	unsafe {
		write_u64(addr, vector.header.begin as u64)?;
		write_u64(add_offset(addr, POINTER_SIZE)?, vector.header.end as u64)?;
		write_u64(
			add_offset(addr, POINTER_SIZE * 2)?,
			vector.header.capacity as u64,
		)?;
	}
	Ok(())
}

fn pointer_elements(vector: NativeVector) -> Result<Vec<usize>, NativeError> {
	let count = vector.checked_len(POINTER_SIZE, MAX_CLAUSES)?;
	let mut elements = Vec::with_capacity(count);
	for index in 0..count {
		elements.push(read_u64(add_offset(vector.header.begin, index * POINTER_SIZE)?)? as usize);
	}
	Ok(elements)
}

fn selection_flag(address: usize) -> Result<u8, NativeError> {
	let flag = read_u8(add_offset(address, SELECTED_OFFSET)?)?;
	if flag > 1 {
		return Err(NativeError::InvalidSelectionFlag);
	}
	Ok(flag)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[derive(Clone)]
	#[repr(C)]
	#[cfg_attr(not(windows), allow(dead_code))]
	struct TestTreaty {
		vtable: *const usize,
		padding_08: u64,
		giver: u64,
		receiver: u64,
		selected: u8,
		padding_21: [u8; 7],
		kind: u32,
		padding_2c: u32,
		/// Mirrors the native layout: the clause amount the ducats executor
		/// checks lives at +0x30, one dword before the target.
		amount: i32,
		padding_34: u32,
		target: u64,
		cost: i32,
		padding_44: [u8; 4],
	}

	#[test]
	fn rejects_reversed_and_partial_vector_spans() {
		for vector in [
			NativeVector {
				header: StdVector {
					begin: 0x1000,
					end: 0xff8,
					capacity: 0x1010,
				},
			},
			NativeVector {
				header: StdVector {
					begin: 0x1000,
					end: 0x1018,
					capacity: 0x1010,
				},
			},
			NativeVector {
				header: StdVector {
					begin: 0x1000,
					end: 0x1001,
					capacity: 0x1010,
				},
			},
			NativeVector {
				header: StdVector {
					begin: 0,
					end: 8,
					capacity: 16,
				},
			},
		] {
			assert_eq!(vector.checked_len(8, 4096), Err(NativeError::InvalidVector));
		}
	}

	#[test]
	fn rejects_excessive_native_vector_elements_before_dereferencing() {
		// The shared StdVector bounds element count, not capacity: a valid
		// heap vector above the null page with more elements than the limit
		// is refused before any element is read.
		let vector = NativeVector {
			header: StdVector {
				begin: 0x0000_1000_0000_0000,
				end: 0x0000_1000_0000_0000 + 8193 * 8,
				capacity: 0x0000_1000_0000_0000 + 8193 * 8,
			},
		};
		assert_eq!(vector.checked_len(8, 4096), Err(NativeError::InvalidVector));
	}

	#[test]
	fn rejects_vectors_in_the_null_page_before_dereferencing() {
		// The shared StdVector refuses any non-null begin at or below the
		// first 64 KiB: no valid heap allocation lives there.
		let vector = NativeVector {
			header: StdVector {
				begin: 0x1000,
				end: 0x1008,
				capacity: 0x1010,
			},
		};
		assert_eq!(vector.checked_len(8, 4096), Err(NativeError::InvalidVector));
	}

	#[test]
	fn outer_storage_has_native_size_and_alignment() {
		assert_eq!(std::mem::size_of::<OfferStorage>(), OFFER_SIZE);
		assert_eq!(std::mem::align_of::<OfferStorage>(), 8);
		assert_eq!(std::mem::size_of::<NativeVector>(), VECTOR_SIZE);
		assert_eq!(std::mem::offset_of!(TestTreaty, selected), SELECTED_OFFSET);
		assert_eq!(std::mem::offset_of!(TestTreaty, amount), AMOUNT_OFFSET);
		assert_eq!(std::mem::offset_of!(TestTreaty, giver), GIVER_OFFSET);
		assert_eq!(std::mem::offset_of!(TestTreaty, receiver), RECEIVER_OFFSET);
	}

	#[cfg(windows)]
	mod windows_memory {
		use super::*;
		use std::cell::Cell;
		use std::sync::OnceLock;

		thread_local! {
			static DESTRUCTIONS: Cell<usize> = const { Cell::new(0) };
			static CLONES: Cell<usize> = const { Cell::new(0) };
			static REBUILDS: Cell<usize> = const { Cell::new(0) };
		}

		unsafe extern "system" fn test_key(
			treaty: *const u8,
			output: *mut NativeKey,
		) -> *mut NativeKey {
			let treaty = unsafe { &*treaty.cast::<TestTreaty>() };
			unsafe {
				ptr::write(
					output,
					NativeKey {
						kind: treaty.kind,
						padding: 0,
						target: treaty.target,
					},
				)
			};
			output
		}

		fn test_vtable() -> *const usize {
			static VTABLE: OnceLock<[usize; 40]> = OnceLock::new();
			VTABLE
				.get_or_init(|| {
					let mut slots = [0; 40];
					slots[KEY_VTABLE_OFFSET / POINTER_SIZE] = test_key as *const () as usize;
					slots[ADD_COST_VTABLE_OFFSET / POINTER_SIZE] =
						test_add_cost as *const () as usize;
					slots
				})
				.as_ptr()
		}

		unsafe extern "system" fn test_add_cost(
			treaty: *const u8,
			totals: *mut u8,
			_direction: u32,
			negate: u8,
		) {
			let treaty = unsafe { &*treaty.cast::<TestTreaty>() };
			let value = if negate != 0 {
				-treaty.cost
			} else {
				treaty.cost
			};
			unsafe {
				let current = ptr::read_unaligned(totals.add(SCORE_IN_TOTALS).cast::<i32>());
				ptr::write_unaligned(totals.add(SCORE_IN_TOTALS).cast::<i32>(), current + value);
			}
		}

		fn leak_vector<T>(mut values: Vec<T>) -> NativeVector {
			let stride = std::mem::size_of::<T>();
			let begin = values.as_mut_ptr() as usize;
			let header = NativeVector {
				header: StdVector {
					begin,
					end: begin + values.len() * stride,
					capacity: begin + values.capacity() * stride,
				},
			};
			std::mem::forget(values);
			header
		}

		unsafe fn take_vector<T>(vector: NativeVector) -> Vec<T> {
			let header = vector.header;
			if header.begin == 0 {
				return Vec::new();
			}
			let stride = std::mem::size_of::<T>();
			unsafe {
				Vec::from_raw_parts(
					header.begin as *mut T,
					(header.end - header.begin) / stride,
					(header.capacity - header.begin) / stride,
				)
			}
		}

		unsafe extern "system" fn test_ctor(
			destination: *mut u8,
			receiver: u64,
			giver: u64,
			direction: bool,
		) -> *mut u8 {
			unsafe {
				ptr::write_bytes(destination, 0, OFFER_SIZE);
				ptr::write_unaligned(destination.add(0x10).cast::<u64>(), giver);
				ptr::write_unaligned(destination.add(0x18).cast::<u64>(), receiver);
				ptr::write(destination.add(0x20), u8::from(direction));
				ptr::write_unaligned(
					destination.add(TABLE_OFFSET).cast::<NativeVector>(),
					leak_vector(vec![NativeVector::default(); KIND_COUNT]),
				);
			}
			destination
		}

		unsafe extern "system" fn test_emplace(
			vector: *mut u8,
			_old_end: *mut u8,
			value: *const *mut u8,
		) -> *mut u8 {
			let header = unsafe { ptr::read_unaligned(vector.cast::<NativeVector>()) };
			let mut values = unsafe { take_vector::<*mut u8>(header) };
			values.push(unsafe { ptr::read(value) });
			let updated = leak_vector(values);
			unsafe { ptr::write_unaligned(vector.cast::<NativeVector>(), updated) };
			(updated.header.end - POINTER_SIZE) as *mut u8
		}

		unsafe extern "system" fn test_clone(source: *const u8) -> *mut u8 {
			CLONES.with(|count| count.set(count.get() + 1));
			Box::into_raw(Box::new(unsafe { (&*source.cast::<TestTreaty>()).clone() })).cast::<u8>()
		}

		unsafe extern "system" fn test_dtor(offer: *mut u8) {
			let owned = unsafe {
				take_vector::<*mut u8>(
					read_vector(offer, OWNED_OFFSET).expect("valid owned vector"),
				)
			};
			for treaty in owned {
				unsafe { drop(Box::from_raw(treaty.cast::<TestTreaty>())) };
				DESTRUCTIONS.with(|count| count.set(count.get() + 1));
			}
			let table = unsafe {
				take_vector::<NativeVector>(
					read_vector(offer, TABLE_OFFSET).expect("valid table vector"),
				)
			};
			for bucket in table {
				unsafe { drop(take_vector::<*mut u8>(bucket)) };
			}
		}

		unsafe extern "system" fn test_rebuild(offer: *mut u8) {
			REBUILDS.with(|count| count.set(count.get() + 1));
			// The native rebuild accumulates every selected clause positively
			// into the cached totals; mirror that with the fixture's costs.
			let mut total = 0;
			if let Ok(owned) = read_vector(offer, OWNED_OFFSET).and_then(pointer_elements) {
				for address in owned {
					let treaty = unsafe { &*(address as *const TestTreaty) };
					if treaty.selected == 1 {
						total += treaty.cost;
					}
				}
			}
			unsafe {
				ptr::write_unaligned(
					offer.add(TOTALS_OFFSET + SCORE_IN_TOTALS).cast::<i32>(),
					total,
				)
			};
		}

		unsafe extern "system" fn test_copy(destination: *mut u8, source: *const u8) -> *mut u8 {
			let engine = test_engine();
			unsafe {
				test_ctor(
					destination,
					ptr::read_unaligned(source.add(0x18).cast::<u64>()),
					ptr::read_unaligned(source.add(0x10).cast::<u64>()),
					ptr::read(source.add(0x20)) != 0,
				);
				let owned = read_vector(source, OWNED_OFFSET)
					.and_then(pointer_elements)
					.expect("valid test offer");
				let table = read_vector(destination, TABLE_OFFSET).expect("valid test table");
				for address in owned {
					let clone = test_clone(address as *const u8);
					engine
						.push_pointer(destination.add(OWNED_OFFSET), clone)
						.expect("valid test ownership vector");
					let treaty = &*clone.cast::<TestTreaty>();
					if treaty.selected == 1 {
						engine
							.push_pointer(
								(table.header.begin + treaty.kind as usize * VECTOR_SIZE)
									as *mut u8,
								clone,
							)
							.expect("valid test selection vector");
					}
				}
			}
			destination
		}

		fn test_engine() -> Engine {
			Engine {
				base: 1,
				functions: NativeFunctions {
					ctor: test_ctor,
					copy_ctor: test_copy,
					dtor: test_dtor,
					clone_treaty: test_clone,
					emplace: test_emplace,
					rebuild: test_rebuild,
				},
				_not_send: PhantomData,
			}
		}

		fn offer(engine: &Engine, giver: u64, receiver: u64) -> OwnedOffer {
			unsafe { engine.new_offer(giver, receiver, true).unwrap() }
		}

		fn add_clause(
			engine: &Engine,
			offer: &mut OwnedOffer,
			kind: u32,
			target: u64,
			selected: bool,
		) -> Clause {
			add_priced_clause(engine, offer, kind, target, selected, 100)
		}

		fn add_priced_clause(
			engine: &Engine,
			offer: &mut OwnedOffer,
			kind: u32,
			target: u64,
			selected: bool,
			cost: i32,
		) -> Clause {
			add_party_clause(engine, offer, kind, target, selected, cost, (111, 222))
		}

		fn add_party_clause(
			engine: &Engine,
			offer: &mut OwnedOffer,
			kind: u32,
			target: u64,
			selected: bool,
			cost: i32,
			parties: (u64, u64),
		) -> Clause {
			let treaty = Box::into_raw(Box::new(TestTreaty {
				vtable: test_vtable(),
				padding_08: 0,
				giver: parties.0,
				receiver: parties.1,
				selected: u8::from(selected),
				padding_21: [0; 7],
				kind,
				padding_2c: 0,
				amount: cost,
				padding_34: 0,
				target,
				cost,
				padding_44: [0; 4],
			}))
			.cast::<u8>();
			unsafe {
				engine
					.push_pointer(offer.as_mut_ptr().add(OWNED_OFFSET), treaty)
					.unwrap();
				if selected {
					let table = read_vector(offer.as_ptr(), TABLE_OFFSET).unwrap();
					engine
						.push_pointer(
							(table.header.begin + kind as usize * VECTOR_SIZE) as *mut u8,
							treaty,
						)
						.unwrap();
				}
			}
			Clause {
				address: treaty as usize,
				kind,
				target,
			}
		}

		#[test]
		fn constructor_preserves_receiver_before_giver_native_argument_order() {
			let engine = test_engine();
			let offer = offer(&engine, 0x11223344, 0x55667788);
			let draft = unsafe { engine.read_draft(offer.as_ptr()).unwrap() };
			assert_eq!((draft.giver, draft.receiver), (0x11223344, 0x55667788));
			assert_eq!(unsafe { ptr::read(offer.as_ptr().add(0x20)) }, 1);
			assert!(draft.selected.is_empty());
		}

		#[test]
		fn reads_only_selected_owned_treaties_and_preserves_the_native_target() {
			let engine = test_engine();
			let mut offer = offer(&engine, 10, 20);
			let selected = add_clause(&engine, &mut offer, 3, 0x123456789abcdef0, true);
			add_clause(&engine, &mut offer, 2, 99, false);
			assert_eq!(
				unsafe { engine.read_draft(offer.as_ptr()).unwrap().selected },
				[selected]
			);
		}

		#[test]
		fn requires_exactly_forty_native_table_vectors() {
			let engine = test_engine();
			let mut offer = offer(&engine, 10, 20);
			let original = read_vector(offer.as_ptr(), TABLE_OFFSET).unwrap();
			unsafe {
				ptr::write_unaligned(
					offer.as_mut_ptr().add(TABLE_OFFSET).cast::<NativeVector>(),
					NativeVector {
						header: StdVector {
							end: original.header.end - VECTOR_SIZE,
							..original.header
						},
					},
				)
			};
			assert_eq!(
				unsafe { engine.read_draft(offer.as_ptr()) },
				Err(NativeError::InvalidTableLength)
			);
			unsafe {
				ptr::write_unaligned(
					offer.as_mut_ptr().add(TABLE_OFFSET).cast::<NativeVector>(),
					original,
				)
			};
		}

		#[test]
		fn rejects_an_indexed_clause_with_the_wrong_native_kind() {
			let engine = test_engine();
			let mut offer = offer(&engine, 10, 20);
			let clause = add_clause(&engine, &mut offer, 3, 42, true);
			unsafe { (*(clause.address as *mut TestTreaty)).kind = 4 };
			assert_eq!(
				unsafe { engine.read_draft(offer.as_ptr()) },
				Err(NativeError::KindMismatch)
			);
		}

		#[test]
		fn rejects_broken_flag_and_index_consistency() {
			let engine = test_engine();
			let mut offer = offer(&engine, 10, 20);
			let clause = add_clause(&engine, &mut offer, 3, 42, true);
			unsafe { ptr::write((clause.address as *mut u8).add(SELECTED_OFFSET), 0) };
			assert_eq!(
				unsafe { engine.read_draft(offer.as_ptr()) },
				Err(NativeError::IndexedClauseNotSelected)
			);
			unsafe { ptr::write((clause.address as *mut u8).add(SELECTED_OFFSET), 2) };
			assert_eq!(
				unsafe { engine.read_draft(offer.as_ptr()) },
				Err(NativeError::InvalidSelectionFlag)
			);
		}

		#[test]
		fn rejects_selected_owned_treaties_missing_from_the_table() {
			let engine = test_engine();
			let mut offer = offer(&engine, 10, 20);
			let clause = add_clause(&engine, &mut offer, 3, 42, false);
			unsafe { ptr::write((clause.address as *mut u8).add(SELECTED_OFFSET), 1) };
			assert_eq!(
				unsafe { engine.read_draft(offer.as_ptr()) },
				Err(NativeError::SelectionNotIndexed)
			);
		}

		#[test]
		fn foreign_or_changed_selection_identity_is_rejected_before_mutation() {
			let engine = test_engine();
			let mut first = offer(&engine, 10, 20);
			let original = add_clause(&engine, &mut first, 3, 42, true);
			let mut second = offer(&engine, 20, 10);
			let foreign = add_clause(&engine, &mut second, 3, 43, true);
			assert_eq!(
				unsafe { engine.replace_selection(first.as_mut_ptr(), &[foreign]) },
				Err(NativeError::ClauseNotOwned)
			);
			assert_eq!(
				unsafe {
					engine.replace_selection(
						first.as_mut_ptr(),
						&[Clause {
							target: 99,
							..original
						}],
					)
				},
				Err(NativeError::ClauseIdentityMismatch)
			);
			assert_eq!(
				unsafe { engine.replace_selection(first.as_mut_ptr(), &[original, original]) },
				Err(NativeError::DuplicateClause)
			);
			assert_eq!(
				unsafe { engine.read_draft(first.as_ptr()).unwrap().selected },
				[original]
			);
		}

		#[test]
		fn filtering_by_address_clears_flags_and_tables_without_releasing_ownership() {
			let engine = test_engine();
			let mut offer = offer(&engine, 10, 20);
			let first = add_clause(&engine, &mut offer, 3, 42, true);
			let second = add_clause(&engine, &mut offer, 3, 43, true);
			unsafe {
				engine
					.retain_selected(offer.as_mut_ptr(), |clause| {
						clause.address == second.address
					})
					.unwrap()
			};
			assert_eq!(
				unsafe { engine.read_draft(offer.as_ptr()).unwrap().selected },
				[second]
			);
			assert_eq!(selection_flag(first.address).unwrap(), 0);
			assert_eq!(
				read_vector(offer.as_ptr(), OWNED_OFFSET)
					.and_then(pointer_elements)
					.unwrap()
					.len(),
				2
			);
			assert_eq!(
				unsafe { (*(second.address as *const TestTreaty)).giver },
				111
			);
		}

		#[test]
		fn reselecting_an_owned_clause_rebuilds_a_valid_index() {
			let engine = test_engine();
			let mut offer = offer(&engine, 10, 20);
			let clause = add_clause(&engine, &mut offer, 39, 42, false);
			unsafe {
				engine
					.replace_selection(offer.as_mut_ptr(), &[clause])
					.unwrap()
			};
			assert_eq!(
				unsafe { engine.read_draft(offer.as_ptr()).unwrap().selected },
				[clause]
			);
		}

		#[test]
		fn appending_deep_clones_selected_terms_and_each_offer_destroys_only_its_own_allocations() {
			let engine = test_engine();
			let before = DESTRUCTIONS.with(Cell::get);
			let clone_count = CLONES.with(Cell::get);
			let mut source = offer(&engine, 10, 20);
			let selected = add_clause(&engine, &mut source, 3, 42, true);
			add_clause(&engine, &mut source, 4, 43, false);
			let mut destination = offer(&engine, 20, 10);
			let result = unsafe {
				engine
					.append_selected_from(destination.as_mut_ptr(), source.as_ptr())
					.unwrap()
			};
			assert_eq!(result.len(), 1);
			assert_ne!(result[0].address, selected.address);
			assert_eq!((result[0].kind, result[0].target), (3, 42));
			assert_eq!(CLONES.with(Cell::get) - clone_count, 1);
			assert_eq!(
				unsafe { engine.read_draft(source.as_ptr()).unwrap().selected },
				[selected]
			);
			drop(source);
			assert_eq!(
				unsafe { engine.read_draft(destination.as_ptr()).unwrap().selected },
				result
			);
			drop(destination);
			assert_eq!(DESTRUCTIONS.with(Cell::get) - before, 3);
		}

		#[test]
		fn native_copy_constructor_creates_disjoint_ownership() {
			let engine = test_engine();
			let mut source = offer(&engine, 10, 20);
			let original = add_clause(&engine, &mut source, 3, 42, true);
			add_clause(&engine, &mut source, 4, 43, false);
			let copy = unsafe { engine.copy_offer(source.as_ptr()).unwrap() };
			let draft = unsafe { engine.read_draft(copy.as_ptr()).unwrap() };
			assert_eq!((draft.giver, draft.receiver), (10, 20));
			assert_ne!(draft.selected[0].address, original.address);
			drop(source);
			assert_eq!(unsafe { engine.read_draft(copy.as_ptr()).unwrap() }, draft);
		}

		#[test]
		fn rejects_native_clone_aliases_without_mutating_or_double_owning_the_source() {
			unsafe extern "system" fn alias(source: *const u8) -> *mut u8 {
				source.cast_mut()
			}
			let mut engine = test_engine();
			let mut source = offer(&engine, 10, 20);
			let original = add_clause(&engine, &mut source, 3, 42, true);
			let mut destination = offer(&engine, 20, 10);
			engine.functions.clone_treaty = alias;
			assert_eq!(
				unsafe { engine.append_selected_from(destination.as_mut_ptr(), source.as_ptr()) },
				Err(NativeError::CloneAliased)
			);
			assert_eq!(
				unsafe { engine.read_draft(source.as_ptr()).unwrap().selected },
				[original]
			);
			assert!(unsafe {
				engine
					.read_draft(destination.as_ptr())
					.unwrap()
					.selected
					.is_empty()
			});
		}

		#[test]
		fn an_invalid_new_clone_remains_owned_but_unselected_for_destruction() {
			unsafe extern "system" fn bad_clone(source: *const u8) -> *mut u8 {
				let cloned = unsafe { test_clone(source) };
				unsafe { (*cloned.cast::<TestTreaty>()).kind = 40 };
				cloned
			}
			let mut engine = test_engine();
			let before = DESTRUCTIONS.with(Cell::get);
			let mut source = offer(&engine, 10, 20);
			add_clause(&engine, &mut source, 3, 42, true);
			let mut destination = offer(&engine, 20, 10);
			engine.functions.clone_treaty = bad_clone;
			assert_eq!(
				unsafe { engine.append_selected_from(destination.as_mut_ptr(), source.as_ptr()) },
				Err(NativeError::InvalidKind)
			);
			assert!(unsafe {
				engine
					.read_draft(destination.as_ptr())
					.unwrap()
					.selected
					.is_empty()
			});
			drop(destination);
			drop(source);
			assert_eq!(DESTRUCTIONS.with(Cell::get) - before, 2);
		}

		#[test]
		fn score_total_reads_the_rebuilt_positive_sum() {
			let engine = test_engine();
			let mut offer = offer(&engine, 10, 20);
			let first = add_priced_clause(&engine, &mut offer, 3, 42, true, 100);
			let second = add_priced_clause(&engine, &mut offer, 4, 43, true, 250);
			add_priced_clause(&engine, &mut offer, 5, 44, false, 999);
			unsafe {
				engine
					.replace_selection(offer.as_mut_ptr(), &[first, second])
					.unwrap()
			};
			assert_eq!(unsafe { engine.score_total(offer.as_ptr()) }, Ok(350));
		}

		#[test]
		fn negating_a_merged_side_twice_yields_the_net_deal_value() {
			let engine = test_engine();
			let mut source = offer(&engine, 20, 10);
			add_priced_clause(&engine, &mut source, 0, 7, true, 60);
			let mut destination = offer(&engine, 10, 20);
			let keep = add_priced_clause(&engine, &mut destination, 3, 42, true, 100);
			let cloned =
				unsafe { engine.append_selected_from(destination.as_mut_ptr(), source.as_ptr()) }
					.unwrap();
			// The native rebuild added both directions positively: 100 + 60.
			assert_eq!(unsafe { engine.score_total(destination.as_ptr()) }, Ok(160));
			for negate in [true, true] {
				unsafe {
					engine
						.apply_clause_cost(destination.as_mut_ptr(), &cloned[0], 1, negate)
						.unwrap()
				};
			}
			// Net deal value: the demand minus the concession.
			assert_eq!(unsafe { engine.score_total(destination.as_ptr()) }, Ok(40));
			// Restoring the editor's own selection rebuilds its one-direction total.
			unsafe {
				engine
					.replace_selection(destination.as_mut_ptr(), &[keep])
					.unwrap()
			};
			assert_eq!(unsafe { engine.score_total(destination.as_ptr()) }, Ok(100));
		}

		#[test]
		fn repricing_requires_an_identity_the_offer_has_selected() {
			let engine = test_engine();
			let mut first = offer(&engine, 10, 20);
			let selected = add_priced_clause(&engine, &mut first, 3, 42, true, 100);
			let mut second = offer(&engine, 20, 10);
			let foreign = add_priced_clause(&engine, &mut second, 3, 43, true, 50);
			assert_eq!(
				unsafe { engine.apply_clause_cost(first.as_mut_ptr(), &foreign, 1, true) },
				Err(NativeError::ClauseIdentityMismatch)
			);
			assert_eq!(
				unsafe { engine.apply_clause_cost(first.as_mut_ptr(), &selected, 1, true) },
				Ok(())
			);
		}

		#[test]
		fn repair_restores_zeroed_foreign_amounts_from_the_snapshot_twin() {
			let engine = test_engine();
			// The snapshot still carries the concession's amount.
			let mut snapshot = offer(&engine, 10, 20);
			add_party_clause(&engine, &mut snapshot, 0, 0, true, 500, (10, 20));
			// The executed offer's twin arrived with its amount zeroed; an
			// unrelated zero-amount clause with other parties stays untouched.
			let mut executed = offer(&engine, 10, 20);
			let zeroed = add_party_clause(&engine, &mut executed, 0, 0, true, 0, (10, 20));
			add_party_clause(&engine, &mut executed, 0, 0, true, 0, (20, 10));
			assert_eq!(
				unsafe {
					engine.repair_clause_amounts(executed.as_mut_ptr(), snapshot.as_ptr(), &[])
				},
				Ok(1)
			);
			assert_eq!(clause_amount(zeroed.address as *const u8), Ok(500));
		}

		#[test]
		fn repair_leaves_nonzero_amounts_and_party_mismatches_alone() {
			let engine = test_engine();
			let mut snapshot = offer(&engine, 10, 20);
			add_party_clause(&engine, &mut snapshot, 0, 7, true, 500, (30, 40));
			let mut executed = offer(&engine, 10, 20);
			add_party_clause(&engine, &mut executed, 0, 7, true, 200, (10, 20));
			add_party_clause(&engine, &mut executed, 0, 7, true, 0, (10, 20));
			assert_eq!(
				unsafe {
					engine.repair_clause_amounts(executed.as_mut_ptr(), snapshot.as_ptr(), &[])
				},
				Ok(0)
			);
		}

		#[test]
		fn netting_mirrored_ducats_keeps_one_direction_and_zeroes_the_loser() {
			let engine = test_engine();
			let mut merged = offer(&engine, 20, 10);
			let demand = add_party_clause(&engine, &mut merged, 0, 0, true, 1000, (20, 10));
			let concession = add_party_clause(&engine, &mut merged, 0, 0, true, 400, (10, 20));
			let edits = unsafe { engine.net_mirrored_ducats(merged.as_mut_ptr()) }.unwrap();
			assert_eq!(edits.len(), 2);
			assert_eq!(clause_amount(demand.address as *const u8), Ok(600));
			assert_eq!(clause_amount(concession.address as *const u8), Ok(0));
			unsafe { engine.restore_amounts(&edits) };
			assert_eq!(clause_amount(demand.address as *const u8), Ok(1000));
			assert_eq!(clause_amount(concession.address as *const u8), Ok(400));
		}

		#[test]
		fn netting_leaves_unmirrored_clauses_and_reverses_the_loser_direction() {
			let engine = test_engine();
			let mut merged = offer(&engine, 20, 10);
			let demand = add_party_clause(&engine, &mut merged, 0, 0, true, 300, (20, 10));
			let concession = add_party_clause(&engine, &mut merged, 0, 0, true, 900, (10, 20));
			add_party_clause(&engine, &mut merged, 3, 42, true, 100, (20, 10));
			let edits = unsafe { engine.net_mirrored_ducats(merged.as_mut_ptr()) }.unwrap();
			assert_eq!(edits.len(), 2);
			assert_eq!(clause_amount(demand.address as *const u8), Ok(0));
			assert_eq!(clause_amount(concession.address as *const u8), Ok(600));
		}

		#[test]
		fn repair_preserves_directions_netted_to_zero_at_submission() {
			let engine = test_engine();
			let mut snapshot = offer(&engine, 10, 20);
			add_party_clause(&engine, &mut snapshot, 0, 0, true, 400, (10, 20));
			let mut executed = offer(&engine, 10, 20);
			let zeroed = add_party_clause(&engine, &mut executed, 0, 0, true, 0, (10, 20));
			let preserve = vec![(0u32, 10u64, 20u64)];
			assert_eq!(
				unsafe {
					engine.repair_clause_amounts(
						executed.as_mut_ptr(),
						snapshot.as_ptr(),
						&preserve,
					)
				},
				Ok(0)
			);
			assert_eq!(clause_amount(zeroed.address as *const u8), Ok(0));
		}

		#[test]
		fn guarded_memory_rejects_unmapped_null_page_addresses() {
			let engine = test_engine();
			let bad = 0x1000 as *const u8;
			assert_eq!(
				unsafe { engine.score_total(bad) },
				Err(NativeError::MemoryAccess)
			);
			// The shared StdVector refuses the null page before Memory is
			// consulted: the failure surfaces as InvalidVector, proving the
			// pre-validation catches unmapped begins without a fault.
			assert!(matches!(
				unsafe { engine.read_draft(bad) },
				Err(NativeError::MemoryAccess | NativeError::InvalidVector)
			));
			assert_eq!(amount_of(0x1000), None);
			assert_eq!(clause_parties(0x1000), Err(NativeError::MemoryAccess));
			assert_eq!(offer_direction(bad), Err(NativeError::MemoryAccess));
		}
	}
}
