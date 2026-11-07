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

use crate::composition::{Clause, Draft};

pub type CountryRef = u64;
pub const OFFER_SIZE: usize = 0x1c0;
const OWNED_OFFSET: usize = 0x88;
const TABLE_OFFSET: usize = 0xd0;
const SELECTED_OFFSET: usize = 0x20;
const KEY_VTABLE_OFFSET: usize = 0x78;
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

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct NativeVector {
	begin: usize,
	end: usize,
	capacity: usize,
}

impl NativeVector {
	fn checked_len(self, stride: usize, limit: usize) -> Result<usize, NativeError> {
		if self.begin == 0 && self.end == 0 && self.capacity == 0 {
			return Ok(0);
		}
		if self.begin == 0
			|| !self.begin.is_multiple_of(POINTER_SIZE)
			|| self.end < self.begin
			|| self.capacity < self.end
			|| !(self.end - self.begin).is_multiple_of(stride)
			|| !(self.capacity - self.begin).is_multiple_of(stride)
		{
			return Err(NativeError::InvalidVector);
		}
		if (self.capacity - self.begin) / stride > limit {
			return Err(NativeError::TooManyClauses);
		}
		Ok((self.end - self.begin) / stride)
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
		Ok(Draft {
			giver: unsafe { ptr::read_unaligned(offer.add(0x10).cast::<CountryRef>()) },
			receiver: unsafe { ptr::read_unaligned(offer.add(0x18).cast::<CountryRef>()) },
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
			unsafe { ptr::write((address as *mut u8).add(SELECTED_OFFSET), 0) };
		}
		let table = unsafe { read_vector(offer.cast_const(), TABLE_OFFSET) };
		for (kind, bucket) in snapshot.buckets.iter().enumerate() {
			let header = (table.begin + kind * VECTOR_SIZE) as *mut NativeVector;
			unsafe {
				ptr::write_unaligned(
					header,
					NativeVector {
						end: bucket.begin,
						..*bucket
					},
				)
			};
		}
		for clause in clauses {
			let header = (table.begin + clause.kind as usize * VECTOR_SIZE) as *mut u8;
			unsafe { self.push_pointer(header, clause.address as *mut u8)? };
			unsafe { ptr::write((clause.address as *mut u8).add(SELECTED_OFFSET), 1) };
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
		let table = unsafe { read_vector(destination, TABLE_OFFSET) };
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
				unsafe { check_pointer(cloned)? };
				// Keep every accepted native allocation under exactly one native
				// destructor, including a clone whose returned key proves invalid.
				unsafe { ptr::write(cloned.add(SELECTED_OFFSET), 0) };
				unsafe { self.push_pointer(destination.add(OWNED_OFFSET), cloned)? };
				let identity = unsafe { self.clause(cloned)? };
				if identity.kind != clause.kind || identity.target != clause.target {
					return Err(NativeError::ClauseIdentityMismatch);
				}
				let header = (table.begin + identity.kind as usize * VECTOR_SIZE) as *mut u8;
				unsafe { self.push_pointer(header, cloned)? };
				unsafe { ptr::write(cloned.add(SELECTED_OFFSET), 1) };
				appended.push(identity);
			}
			Ok(appended)
		})();
		unsafe { (self.functions.rebuild)(destination) };
		result
	}

	unsafe fn snapshot(&self, offer: *const u8) -> Result<Snapshot, NativeError> {
		unsafe { check_pointer(offer)? };
		let owned = unsafe { pointer_elements(read_vector(offer, OWNED_OFFSET))? };
		let mut owned_set = BTreeSet::new();
		for &address in &owned {
			if address == 0 {
				return Err(NativeError::NullClause);
			}
			unsafe { check_pointer(address as *const u8)? };
			if !owned_set.insert(address) {
				return Err(NativeError::DuplicateClause);
			}
		}
		let table = unsafe { read_vector(offer, TABLE_OFFSET) };
		if table.checked_len(VECTOR_SIZE, KIND_COUNT)? != KIND_COUNT {
			return Err(NativeError::InvalidTableLength);
		}
		let mut buckets = [NativeVector::default(); KIND_COUNT];
		let mut indexed = BTreeSet::new();
		let mut selected = Vec::new();
		for (kind, bucket) in buckets.iter_mut().enumerate() {
			*bucket = unsafe {
				ptr::read_unaligned((table.begin + kind * VECTOR_SIZE) as *const NativeVector)
			};
			for address in unsafe { pointer_elements(*bucket)? } {
				if address == 0 {
					return Err(NativeError::NullClause);
				}
				if !owned_set.contains(&address) {
					return Err(NativeError::ClauseNotOwned);
				}
				if !indexed.insert(address) {
					return Err(NativeError::DuplicateClause);
				}
				if unsafe { selection_flag(address)? } != 1 {
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
			if unsafe { selection_flag(address)? } == 1 && !indexed.contains(&address) {
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
		unsafe { check_pointer(treaty)? };
		let vtable = unsafe { ptr::read_unaligned(treaty.cast::<usize>()) };
		unsafe { check_pointer(vtable as *const u8)? };
		let slot = vtable
			.checked_add(KEY_VTABLE_OFFSET)
			.ok_or(NativeError::AddressOverflow)?;
		let method = unsafe { ptr::read_unaligned(slot as *const usize) };
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
		let header = unsafe { ptr::read_unaligned(vector.cast::<NativeVector>()) };
		let previous_len = header.checked_len(POINTER_SIZE, MAX_CLAUSES)?;
		if previous_len == MAX_CLAUSES {
			return Err(NativeError::TooManyClauses);
		}
		if header.end == header.capacity {
			unsafe { (self.functions.emplace)(vector, header.end as *mut u8, &value) };
		} else {
			unsafe { ptr::write_unaligned(header.end as *mut *mut u8, value) };
			let next_end = header
				.end
				.checked_add(POINTER_SIZE)
				.ok_or(NativeError::AddressOverflow)?;
			unsafe {
				ptr::write_unaligned(
					vector.cast::<NativeVector>(),
					NativeVector {
						end: next_end,
						..header
					},
				)
			};
		}
		let updated = unsafe { ptr::read_unaligned(vector.cast::<NativeVector>()) };
		if updated.checked_len(POINTER_SIZE, MAX_CLAUSES)? != previous_len + 1
			|| unsafe { ptr::read_unaligned((updated.end - POINTER_SIZE) as *const *mut u8) }
				!= value
		{
			return Err(NativeError::NativeVectorMutation);
		}
		Ok(())
	}
}

unsafe fn check_pointer(pointer: *const u8) -> Result<(), NativeError> {
	if pointer.is_null() {
		Err(NativeError::NullPointer)
	} else if !(pointer as usize).is_multiple_of(POINTER_SIZE) {
		Err(NativeError::MisalignedPointer)
	} else {
		Ok(())
	}
}

unsafe fn read_vector(offer: *const u8, offset: usize) -> NativeVector {
	unsafe { ptr::read_unaligned(offer.add(offset).cast::<NativeVector>()) }
}

unsafe fn pointer_elements(vector: NativeVector) -> Result<Vec<usize>, NativeError> {
	let count = vector.checked_len(POINTER_SIZE, MAX_CLAUSES)?;
	let mut elements = Vec::with_capacity(count);
	for index in 0..count {
		elements.push(unsafe {
			ptr::read_unaligned((vector.begin + index * POINTER_SIZE) as *const usize)
		});
	}
	Ok(elements)
}

unsafe fn selection_flag(address: usize) -> Result<u8, NativeError> {
	let flag = unsafe { ptr::read((address as *const u8).add(SELECTED_OFFSET)) };
	if flag > 1 {
		return Err(NativeError::InvalidSelectionFlag);
	}
	Ok(flag)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::cell::Cell;
	use std::sync::OnceLock;

	thread_local! {
		static DESTRUCTIONS: Cell<usize> = const { Cell::new(0) };
		static CLONES: Cell<usize> = const { Cell::new(0) };
		static REBUILDS: Cell<usize> = const { Cell::new(0) };
	}

	#[derive(Clone)]
	#[repr(C)]
	struct TestTreaty {
		vtable: *const usize,
		padding_08: u64,
		giver: u64,
		receiver: u64,
		selected: u8,
		padding_21: [u8; 7],
		kind: u32,
		padding_2c: u32,
		target: u64,
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
		static VTABLE: OnceLock<[usize; 16]> = OnceLock::new();
		VTABLE
			.get_or_init(|| {
				let mut slots = [0; 16];
				slots[KEY_VTABLE_OFFSET / POINTER_SIZE] = test_key as *const () as usize;
				slots
			})
			.as_ptr()
	}

	fn leak_vector<T>(mut values: Vec<T>) -> NativeVector {
		let stride = std::mem::size_of::<T>();
		let begin = values.as_mut_ptr() as usize;
		let header = NativeVector {
			begin,
			end: begin + values.len() * stride,
			capacity: begin + values.capacity() * stride,
		};
		std::mem::forget(values);
		header
	}

	unsafe fn take_vector<T>(header: NativeVector) -> Vec<T> {
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
		(updated.end - POINTER_SIZE) as *mut u8
	}

	unsafe extern "system" fn test_clone(source: *const u8) -> *mut u8 {
		CLONES.with(|count| count.set(count.get() + 1));
		Box::into_raw(Box::new(unsafe { (&*source.cast::<TestTreaty>()).clone() })).cast::<u8>()
	}

	unsafe extern "system" fn test_dtor(offer: *mut u8) {
		let owned = unsafe { take_vector::<*mut u8>(read_vector(offer, OWNED_OFFSET)) };
		for treaty in owned {
			unsafe { drop(Box::from_raw(treaty.cast::<TestTreaty>())) };
			DESTRUCTIONS.with(|count| count.set(count.get() + 1));
		}
		let table = unsafe { take_vector::<NativeVector>(read_vector(offer, TABLE_OFFSET)) };
		for bucket in table {
			unsafe { drop(take_vector::<*mut u8>(bucket)) };
		}
	}

	unsafe extern "system" fn test_rebuild(_offer: *mut u8) {
		REBUILDS.with(|count| count.set(count.get() + 1));
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
			let owned =
				pointer_elements(read_vector(source, OWNED_OFFSET)).expect("valid test offer");
			let table = read_vector(destination, TABLE_OFFSET);
			for address in owned {
				let clone = test_clone(address as *const u8);
				engine
					.push_pointer(destination.add(OWNED_OFFSET), clone)
					.expect("valid test ownership vector");
				let treaty = &*clone.cast::<TestTreaty>();
				if treaty.selected == 1 {
					engine
						.push_pointer(
							(table.begin + treaty.kind as usize * VECTOR_SIZE) as *mut u8,
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
		let treaty = Box::into_raw(Box::new(TestTreaty {
			vtable: test_vtable(),
			padding_08: 0,
			giver: 111,
			receiver: 222,
			selected: u8::from(selected),
			padding_21: [0; 7],
			kind,
			padding_2c: 0,
			target,
		}))
		.cast::<u8>();
		unsafe {
			engine
				.push_pointer(offer.as_mut_ptr().add(OWNED_OFFSET), treaty)
				.unwrap();
			if selected {
				let table = read_vector(offer.as_ptr(), TABLE_OFFSET);
				engine
					.push_pointer(
						(table.begin + kind as usize * VECTOR_SIZE) as *mut u8,
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
	fn rejects_reversed_and_partial_vector_spans() {
		for vector in [
			NativeVector {
				begin: 0x1000,
				end: 0xff8,
				capacity: 0x1010,
			},
			NativeVector {
				begin: 0x1000,
				end: 0x1018,
				capacity: 0x1010,
			},
			NativeVector {
				begin: 0x1000,
				end: 0x1001,
				capacity: 0x1010,
			},
			NativeVector {
				begin: 0,
				end: 8,
				capacity: 16,
			},
		] {
			assert_eq!(vector.checked_len(8, 4096), Err(NativeError::InvalidVector));
		}
	}

	#[test]
	fn rejects_excessive_native_vector_capacity_before_dereferencing() {
		let vector = NativeVector {
			begin: 0x1000,
			end: 0x1008,
			capacity: 0x1000 + 8192 * 8,
		};
		assert_eq!(
			vector.checked_len(8, 4096),
			Err(NativeError::TooManyClauses)
		);
	}

	#[test]
	fn outer_storage_has_native_size_and_alignment() {
		assert_eq!(std::mem::size_of::<OfferStorage>(), OFFER_SIZE);
		assert_eq!(std::mem::align_of::<OfferStorage>(), 8);
		assert_eq!(std::mem::size_of::<NativeVector>(), VECTOR_SIZE);
		assert_eq!(std::mem::offset_of!(TestTreaty, selected), SELECTED_OFFSET);
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
		let original = unsafe { read_vector(offer.as_ptr(), TABLE_OFFSET) };
		unsafe {
			ptr::write_unaligned(
				offer.as_mut_ptr().add(TABLE_OFFSET).cast::<NativeVector>(),
				NativeVector {
					end: original.end - VECTOR_SIZE,
					..original
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
		assert_eq!(unsafe { selection_flag(first.address).unwrap() }, 0);
		assert_eq!(
			unsafe {
				pointer_elements(read_vector(offer.as_ptr(), OWNED_OFFSET))
					.unwrap()
					.len()
			},
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
}
