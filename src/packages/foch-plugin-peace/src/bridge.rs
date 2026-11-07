//! Redirect verified existing instructions through nearby absolute jump thunks.
//!
//! Only the four displacement bytes are registered with the host. No game
//! function prologue is relocated. Successful installations retain their
//! executable allocations until process exit, matching the plugin's lifetime.

use core::ffi::c_void;
use foch_plugin_abi::{self as abi, FochHostApi, FochPatch};

/// One exact CALL rel32 or `48 8D /r` RIP-relative LEA to redirect.
///
/// A LEA still loads a function pointer into its original destination register;
/// that pointer now addresses an absolute jump thunk for `callback`.
pub struct Redirect<'a> {
	pub site_rva: usize,
	pub expected_target_rva: usize,
	pub original: &'a [u8],
	pub callback: *const c_void,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BridgeError {
	pub code: i32,
	pub site_rva: usize,
	pub detail: &'static str,
	/// At least one host restore failed. Its thunk is retained rather than freed.
	pub rollback_failed: bool,
}

impl core::fmt::Display for BridgeError {
	fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		write!(
			formatter,
			"{} at RVA {:#x} (host code {}, rollback failed: {})",
			self.detail, self.site_rva, self.code, self.rollback_failed
		)
	}
}

impl std::error::Error for BridgeError {}

fn bridge_error(site_rva: usize, code: i32, detail: &'static str) -> BridgeError {
	BridgeError {
		code,
		site_rva,
		detail,
		rollback_failed: false,
	}
}

/// Prepare every redirect, then enable the batch. At most 64 sites are accepted.
///
/// On an enable failure, earlier patches are disabled in reverse order. A failed
/// restore is reported and its thunk remains allocated. Disabled registrations
/// remain host-owned because ABI 1.0 has no patch-unregister operation.
///
/// # Safety
/// Call during plugin initialization, before game threads can execute these
/// sites. `host` must point to a live ABI table and its declared executable image
/// must be readable for its full size. No other thread may mutate the supplied
/// instructions during installation. Each callback must have the original
/// target's calling convention/signature and remain executable until process
/// exit. The plugin must not unload after a successful or partially rolled-back
/// installation.
pub unsafe fn install(
	host: *const FochHostApi,
	redirects: &[Redirect<'_>],
) -> Result<(), BridgeError> {
	if host.is_null() {
		return Err(bridge_error(0, abi::result::E_INVALID, "null host table"));
	}
	let api = unsafe { &*host };
	if api.size < core::mem::size_of::<FochHostApi>() as u32 || api.abi_major != abi::ABI_MAJOR {
		return Err(bridge_error(
			0,
			abi::result::E_UNSUPPORTED,
			"incompatible host table",
		));
	}
	if api.exe_base.is_null()
		|| api.exe_size == 0
		|| (api.exe_base as usize).checked_add(api.exe_size).is_none()
	{
		return Err(bridge_error(
			0,
			abi::result::E_INVALID,
			"invalid game image extent",
		));
	}
	if redirects.len() > 64 {
		return Err(bridge_error(
			0,
			abi::result::E_INVALID,
			"redirect batch exceeds 64 instructions",
		));
	}

	// Validate all sites before creating any host registration or allocation.
	for (index, redirect) in redirects.iter().enumerate() {
		unsafe { check_redirect(api, redirect)? };
		let end = redirect.site_rva + redirect.original.len();
		for previous in &redirects[..index] {
			if redirect.site_rva < previous.site_rva + previous.original.len()
				&& previous.site_rva < end
			{
				return Err(bridge_error(
					redirect.site_rva,
					abi::result::E_CONFLICT,
					"redirect instructions overlap",
				));
			}
		}
	}

	let mut prepared = Vec::with_capacity(redirects.len());
	for redirect in redirects {
		let (site, encoding) = unsafe { check_redirect(api, redirect)? };
		let end = site + encoding.len;
		let thunk = Thunk::allocate_near(end, redirect.callback as usize)
			.map_err(|detail| bridge_error(redirect.site_rva, abi::result::E_MEMORY, detail))?;
		let bytes = relative_displacement(end, thunk.address as usize)
			.map_err(|detail| bridge_error(redirect.site_rva, abi::result::E_MEMORY, detail))?;
		let mut patch = core::ptr::null_mut();
		let code = unsafe {
			(api.patch_create)(
				host,
				(site + encoding.displacement_offset) as *mut c_void,
				bytes.as_ptr(),
				bytes.len(),
				&mut patch,
			)
		};
		if code != abi::result::OK {
			return Err(bridge_error(
				redirect.site_rva,
				code,
				"host rejected displacement patch registration",
			));
		}
		if patch.is_null() {
			return Err(bridge_error(
				redirect.site_rva,
				abi::result::E_INVALID,
				"host returned a null patch handle",
			));
		}
		prepared.push(PreparedRedirect {
			host,
			patch,
			thunk: Some(thunk),
			enabled: false,
			site,
			original: redirect.original,
		});
	}

	if let Err(error) = enable_batch(prepared.len(), |index, enabled| {
		let redirect = &mut prepared[index];
		if enabled {
			let current = unsafe {
				core::slice::from_raw_parts(redirect.site as *const u8, redirect.original.len())
			};
			if current != redirect.original {
				return abi::result::E_CONFLICT;
			}
		}
		let code = unsafe { (api.patch_set_enabled)(host, redirect.patch, i32::from(enabled)) };
		if code == abi::result::OK {
			redirect.enabled = enabled;
		}
		code
	}) {
		return Err(BridgeError {
			code: error.code,
			site_rva: redirects[error.index].site_rva,
			detail: "host could not enable the complete redirect batch",
			rollback_failed: error.rollback_failed,
		});
	}
	for redirect in prepared {
		redirect.persist();
	}
	Ok(())
}

unsafe fn check_redirect(
	api: &FochHostApi,
	redirect: &Redirect<'_>,
) -> Result<(usize, InstructionEncoding), BridgeError> {
	if redirect.callback.is_null() {
		return Err(bridge_error(
			redirect.site_rva,
			abi::result::E_INVALID,
			"null callback address",
		));
	}
	// This first pass bounds the raw memory read and validates the original target.
	let encoding = validate_instruction(
		redirect.site_rva,
		redirect.expected_target_rva,
		redirect.original,
		redirect.original,
		api.exe_size,
	)
	.map_err(|detail| bridge_error(redirect.site_rva, abi::result::E_UNSUPPORTED, detail))?;
	let site = api.exe_base as usize + redirect.site_rva;
	let current = unsafe { core::slice::from_raw_parts(site as *const u8, encoding.len) };
	validate_instruction(
		redirect.site_rva,
		redirect.expected_target_rva,
		redirect.original,
		current,
		api.exe_size,
	)
	.map_err(|detail| bridge_error(redirect.site_rva, abi::result::E_CONFLICT, detail))?;
	Ok((site, encoding))
}

struct PreparedRedirect<'a> {
	host: *const FochHostApi,
	patch: *mut FochPatch,
	thunk: Option<Thunk>,
	enabled: bool,
	site: usize,
	original: &'a [u8],
}

impl PreparedRedirect<'_> {
	fn persist(mut self) {
		if let Some(thunk) = self.thunk.take() {
			core::mem::forget(thunk);
		}
		// Ownership is intentionally abandoned; the active patch must survive Drop.
		self.enabled = false;
	}
}

impl Drop for PreparedRedirect<'_> {
	fn drop(&mut self) {
		if self.enabled {
			let code = unsafe { ((*self.host).patch_set_enabled)(self.host, self.patch, 0) };
			if code != abi::result::OK {
				// Never free code that a remaining live patch can still reach.
				if let Some(thunk) = self.thunk.take() {
					core::mem::forget(thunk);
				}
			}
		}
	}
}

struct Thunk {
	address: *mut c_void,
}

impl Thunk {
	fn allocate_near(instruction_end: usize, callback: usize) -> Result<Self, &'static str> {
		let mut information: SystemInfo = unsafe { core::mem::zeroed() };
		unsafe { GetSystemInfo(&mut information) };
		let granularity = information.allocation_granularity as usize;
		if !granularity.is_power_of_two() {
			return Err("Windows returned an invalid allocation granularity");
		}
		let minimum = information.minimum_application_address as usize;
		let maximum = (information.maximum_application_address as usize).saturating_sub(13);
		let anchor = instruction_end & !(granularity - 1);
		// One extra allocation slot covers alignment at both signed rel32 limits.
		// This bound caps the search at roughly 65,540 probes with 64 KiB slots.
		let steps = i32::MAX as usize / granularity + 2;
		for step in 0..steps {
			let distance = step * granularity;
			let candidates = [anchor.checked_add(distance), anchor.checked_sub(distance)];
			for (direction, candidate) in candidates.into_iter().enumerate() {
				if step == 0 && direction == 1 {
					continue;
				}
				let Some(candidate) = candidate else { continue };
				if candidate < minimum
					|| candidate > maximum
					|| relative_displacement(instruction_end, candidate).is_err()
				{
					continue;
				}
				let address = unsafe { VirtualAlloc(candidate as *mut c_void, 14, 0x3000, 0x04) };
				if address.is_null() {
					continue;
				}
				let thunk = Self { address };
				if relative_displacement(instruction_end, address as usize).is_err() {
					continue;
				}
				let bytes = absolute_jump(callback);
				unsafe {
					core::ptr::copy_nonoverlapping(bytes.as_ptr(), address.cast(), bytes.len())
				};
				let mut old_protection = 0;
				if unsafe { VirtualProtect(address, bytes.len(), 0x20, &mut old_protection) } == 0 {
					return Err("could not make the jump thunk executable");
				}
				if unsafe { FlushInstructionCache(GetCurrentProcess(), address, bytes.len()) } == 0
				{
					return Err("could not flush the jump thunk instruction cache");
				}
				return Ok(thunk);
			}
		}
		Err("no executable jump-thunk allocation was available within signed rel32 range")
	}
}

impl Drop for Thunk {
	fn drop(&mut self) {
		unsafe { VirtualFree(self.address, 0, 0x8000) };
	}
}

#[repr(C)]
struct SystemInfo {
	processor_architecture: u16,
	reserved: u16,
	page_size: u32,
	minimum_application_address: *mut c_void,
	maximum_application_address: *mut c_void,
	active_processor_mask: usize,
	number_of_processors: u32,
	processor_type: u32,
	allocation_granularity: u32,
	processor_level: u16,
	processor_revision: u16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
	fn GetSystemInfo(information: *mut SystemInfo);
	fn VirtualAlloc(
		address: *mut c_void,
		size: usize,
		allocation_type: u32,
		protection: u32,
	) -> *mut c_void;
	fn VirtualFree(address: *mut c_void, size: usize, free_type: u32) -> i32;
	fn VirtualProtect(
		address: *mut c_void,
		size: usize,
		protection: u32,
		previous: *mut u32,
	) -> i32;
	fn GetCurrentProcess() -> *mut c_void;
	fn FlushInstructionCache(process: *mut c_void, address: *const c_void, size: usize) -> i32;
}

#[derive(Debug, PartialEq, Eq)]
struct InstructionEncoding {
	len: usize,
	displacement_offset: usize,
}

fn relative_displacement(instruction_end: usize, target: usize) -> Result<[u8; 4], &'static str> {
	let displacement = target as i128 - instruction_end as i128;
	i32::try_from(displacement)
		.map(i32::to_le_bytes)
		.map_err(|_| "redirect target is outside signed rel32 range")
}

fn validate_instruction(
	site_rva: usize,
	expected_target_rva: usize,
	expected: &[u8],
	actual: &[u8],
	image_size: usize,
) -> Result<InstructionEncoding, &'static str> {
	let encoding = match expected {
		[0xe8, _, _, _, _] => InstructionEncoding {
			len: 5,
			displacement_offset: 1,
		},
		[0x48, 0x8d, modrm, _, _, _, _] if modrm & 0xc7 == 0x05 => InstructionEncoding {
			len: 7,
			displacement_offset: 3,
		},
		_ => return Err("instruction is neither CALL rel32 nor 48 8D RIP-relative LEA"),
	};
	let end = site_rva
		.checked_add(encoding.len)
		.ok_or("instruction RVA overflows")?;
	if end > image_size || expected_target_rva >= image_size {
		return Err("instruction or expected target is outside the game image");
	}
	if actual != expected {
		return Err("full instruction bytes differ from the expected original");
	}
	let displacement = i32::from_le_bytes(
		expected[encoding.displacement_offset..encoding.displacement_offset + 4]
			.try_into()
			.expect("validated displacement length"),
	);
	let target = end
		.checked_add_signed(displacement as isize)
		.ok_or("instruction target RVA overflows")?;
	if target != expected_target_rva {
		return Err("original instruction does not address the expected target RVA");
	}
	Ok(encoding)
}

fn absolute_jump(callback: usize) -> [u8; 14] {
	let mut bytes = [0xff, 0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
	bytes[6..].copy_from_slice(&(callback as u64).to_le_bytes());
	bytes
}

#[derive(Debug, PartialEq, Eq)]
struct BatchError {
	index: usize,
	code: i32,
	rollback_failed: bool,
}

fn enable_batch(
	count: usize,
	mut set_enabled: impl FnMut(usize, bool) -> i32,
) -> Result<(), BatchError> {
	for index in 0..count {
		let code = set_enabled(index, true);
		if code != 0 {
			let mut rollback_failed = false;
			for previous in (0..index).rev() {
				if set_enabled(previous, false) != 0 {
					rollback_failed = true;
				}
			}
			return Err(BatchError {
				index,
				code,
				rollback_failed,
			});
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn relative_displacement_encodes_forward_and_backward_from_instruction_end() {
		assert_eq!(relative_displacement(0x1005, 0x1010), Ok([0x0b, 0, 0, 0]));
		assert_eq!(
			relative_displacement(0x1005, 0x1000),
			Ok([0xfb, 0xff, 0xff, 0xff])
		);
	}

	#[test]
	fn relative_displacement_accepts_exact_signed_boundaries() {
		assert_eq!(
			relative_displacement(0, i32::MAX as usize),
			Ok(i32::MAX.to_le_bytes())
		);
		assert_eq!(
			relative_displacement(0x8000_0000, 0),
			Ok(i32::MIN.to_le_bytes())
		);
	}

	#[test]
	fn relative_displacement_refuses_out_of_range_addresses() {
		assert!(relative_displacement(0, 0x8000_0000).is_err());
		assert!(relative_displacement(0x8000_0001, 0).is_err());
		assert!(relative_displacement(0, usize::MAX).is_err());
		assert!(relative_displacement(usize::MAX, 0).is_err());
	}

	#[test]
	fn call_validation_checks_full_bytes_and_expected_target() {
		let call = [0xe8, 0x4b, 0, 0, 0];
		assert_eq!(
			validate_instruction(0x100, 0x150, &call, &call, 0x1000),
			Ok(InstructionEncoding {
				len: 5,
				displacement_offset: 1
			})
		);
		assert!(validate_instruction(0x100, 0x151, &call, &call, 0x1000).is_err());
		assert!(validate_instruction(0x100, 0x150, &call, &[0xe9, 0x4b, 0, 0, 0], 0x1000).is_err());
		assert!(validate_instruction(0x100, 0x150, &call, &[0xe8, 0x4c, 0, 0, 0], 0x1000).is_err());
	}

	#[test]
	fn lea_validation_accepts_each_rip_relative_destination_register() {
		for register in 0..8 {
			let lea = [0x48, 0x8d, 0x05 | (register << 3), 0x49, 0, 0, 0];
			assert_eq!(
				validate_instruction(0x100, 0x150, &lea, &lea, 0x1000),
				Ok(InstructionEncoding {
					len: 7,
					displacement_offset: 3
				})
			);
			assert!(validate_instruction(0x100, 0x151, &lea, &lea, 0x1000).is_err());
			let mut changed = lea;
			changed[2] ^= 8;
			assert!(validate_instruction(0x100, 0x150, &lea, &changed, 0x1000).is_err());
		}
	}

	#[test]
	fn instruction_validation_checks_negative_target_displacements() {
		let call = [0xe8, 0xfb, 0xff, 0xff, 0xff];
		assert!(validate_instruction(0x100, 0x100, &call, &call, 0x1000).is_ok());
		let lea = [0x48, 0x8d, 0x15, 0xf9, 0xff, 0xff, 0xff];
		assert!(validate_instruction(0x100, 0x100, &lea, &lea, 0x1000).is_ok());
	}

	#[test]
	fn instruction_validation_refuses_unsupported_and_truncated_instructions() {
		for bytes in [
			&[0xe9, 0, 0, 0, 0][..],
			&[0xe8, 0, 0, 0][..],
			&[0xe8, 0, 0, 0, 0, 0][..],
			&[0x49, 0x8d, 0x0d, 0, 0, 0, 0][..],
			&[0x48, 0x8d, 0x0c, 0, 0, 0, 0][..],
			&[0x48, 0x8d, 0x4d, 0, 0, 0, 0][..],
		] {
			assert!(validate_instruction(0x100, 0x107, bytes, bytes, 0x1000).is_err());
		}
	}

	#[test]
	fn instruction_validation_refuses_image_bounds_and_address_overflow() {
		let call = [0xe8, 0, 0, 0, 0];
		assert!(validate_instruction(usize::MAX - 1, 0, &call, &call, usize::MAX).is_err());
		assert!(validate_instruction(0x100, 0x105, &call, &call, 0x104).is_err());
		assert!(validate_instruction(0x100, 0x105, &call, &call, 0x105).is_err());
		let underflow = [0xe8, 0, 0, 0, 0x80];
		assert!(validate_instruction(0, 0, &underflow, &underflow, 0x1000).is_err());
	}

	#[test]
	fn absolute_jump_preserves_registers_and_encodes_the_full_callback_pointer() {
		assert_eq!(
			absolute_jump(0x1234_5678_9abc_def0),
			[
				0xff, 0x25, 0, 0, 0, 0, 0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12,
			]
		);
	}
	#[test]
	fn batch_enables_every_patch_in_order() {
		let mut calls = Vec::new();
		assert_eq!(
			enable_batch(3, |index, enabled| {
				calls.push((index, enabled));
				0
			}),
			Ok(())
		);
		assert_eq!(calls, [(0, true), (1, true), (2, true)]);
	}

	#[test]
	fn batch_rolls_back_previous_patches_in_reverse_after_enable_failure() {
		let mut calls = Vec::new();
		let result = enable_batch(4, |index, enabled| {
			calls.push((index, enabled));
			if index == 2 && enabled { -3 } else { 0 }
		});
		assert_eq!(
			result,
			Err(BatchError {
				index: 2,
				code: -3,
				rollback_failed: false
			})
		);
		assert_eq!(
			calls,
			[(0, true), (1, true), (2, true), (1, false), (0, false)]
		);
	}

	#[test]
	fn batch_reports_rollback_failure_and_still_attempts_remaining_patches() {
		let mut calls = Vec::new();
		let result = enable_batch(3, |index, enabled| {
			calls.push((index, enabled));
			if index == 2 && enabled {
				-3
			} else if index == 1 && !enabled {
				-4
			} else {
				0
			}
		});
		assert_eq!(
			result,
			Err(BatchError {
				index: 2,
				code: -3,
				rollback_failed: true
			})
		);
		assert_eq!(
			calls,
			[(0, true), (1, true), (2, true), (1, false), (0, false)]
		);
	}

	#[cfg(all(windows, target_arch = "x86_64"))]
	mod executable_buffer {
		use super::*;
		use std::sync::Mutex;

		const IMAGE_SIZE: usize = 4096;
		const FIRST_ENTRY: usize = 0;
		const SECOND_ENTRY: usize = 0x40;
		const LEA_ENTRY: usize = 0x80;
		const ORIGINAL_TARGET: usize = 0x100;
		const ORIGINAL_RESULT: u32 = 0x1357_2468;
		const REPLACEMENT_RESULT: u32 = 0x2468_1357;

		extern "C" fn replacement() -> u32 {
			REPLACEMENT_RESULT
		}

		struct OwnedImage {
			address: *mut u8,
		}

		impl OwnedImage {
			fn new() -> Self {
				let address =
					unsafe { VirtualAlloc(core::ptr::null_mut(), IMAGE_SIZE, 0x3000, 0x04) };
				assert!(
					!address.is_null(),
					"could not allocate the test's own code page"
				);
				let image = Self {
					address: address.cast(),
				};
				unsafe { core::ptr::write_bytes(image.address, 0xcc, IMAGE_SIZE) };
				for entry in [FIRST_ENTRY, SECOND_ENTRY] {
					// Win64: reserve 32 bytes of shadow space and align RSP for CALL.
					let mut bytes = [
						0x48, 0x83, 0xec, 0x28, 0xe8, 0, 0, 0, 0, 0x48, 0x83, 0xc4, 0x28, 0xc3,
					];
					let displacement = (ORIGINAL_TARGET as i32 - (entry + 9) as i32).to_le_bytes();
					bytes[5..9].copy_from_slice(&displacement);
					unsafe {
						core::ptr::copy_nonoverlapping(
							bytes.as_ptr(),
							image.address.add(entry),
							bytes.len(),
						)
					};
				}
				// LEA RAX,[RIP+disp32]; RET returns a callable pointer without a call.
				let mut lea = [0x48, 0x8d, 0x05, 0, 0, 0, 0, 0xc3];
				lea[3..7].copy_from_slice(
					&(ORIGINAL_TARGET as i32 - (LEA_ENTRY + 7) as i32).to_le_bytes(),
				);
				unsafe {
					core::ptr::copy_nonoverlapping(
						lea.as_ptr(),
						image.address.add(LEA_ENTRY),
						lea.len(),
					)
				};
				// MOV EAX,original_result; RET is the original target in this image.
				let mut original = [0xb8, 0, 0, 0, 0, 0xc3];
				original[1..5].copy_from_slice(&ORIGINAL_RESULT.to_le_bytes());
				unsafe {
					core::ptr::copy_nonoverlapping(
						original.as_ptr(),
						image.address.add(ORIGINAL_TARGET),
						original.len(),
					)
				};
				let mut previous = 0;
				assert_ne!(
					unsafe { VirtualProtect(address, IMAGE_SIZE, 0x20, &mut previous) },
					0
				);
				assert_ne!(
					unsafe { FlushInstructionCache(GetCurrentProcess(), address, IMAGE_SIZE) },
					0
				);
				image
			}

			fn snapshot(&self, rva: usize, len: usize) -> Vec<u8> {
				assert!(rva.checked_add(len).is_some_and(|end| end <= IMAGE_SIZE));
				unsafe { core::slice::from_raw_parts(self.address.add(rva), len) }.to_vec()
			}

			fn call(&self, entry: usize) -> u32 {
				assert!([FIRST_ENTRY, SECOND_ENTRY, ORIGINAL_TARGET].contains(&entry));
				let function: unsafe extern "C" fn() -> u32 =
					unsafe { core::mem::transmute(self.address.add(entry)) };
				unsafe { function() }
			}

			fn pointer_from_lea(&self) -> *const c_void {
				let function: unsafe extern "C" fn() -> *const c_void =
					unsafe { core::mem::transmute(self.address.add(LEA_ENTRY)) };
				unsafe { function() }
			}
		}

		impl Drop for OwnedImage {
			fn drop(&mut self) {
				unsafe { VirtualFree(self.address.cast(), 0, 0x8000) };
			}
		}

		#[derive(Debug, PartialEq, Eq)]
		enum HostCall {
			Create(usize),
			Toggle(usize, bool),
		}

		struct TestPatch {
			address: usize,
			original: Vec<u8>,
			bytes: Vec<u8>,
			enabled: bool,
		}

		#[derive(Default)]
		struct HostState {
			patches: Vec<TestPatch>,
			calls: Vec<HostCall>,
			fail_create: Option<usize>,
			fail_enable: Option<usize>,
			fail_disable: Option<usize>,
		}

		// The API is the first field so the callbacks can recover this test context.
		#[repr(C)]
		struct TestHost {
			api: FochHostApi,
			state: Mutex<HostState>,
		}

		impl TestHost {
			fn new(image: &OwnedImage) -> Box<Self> {
				Box::new(Self {
					api: FochHostApi {
						size: core::mem::size_of::<FochHostApi>() as u32,
						abi_major: abi::ABI_MAJOR,
						abi_minor: abi::ABI_MINOR,
						plugin_id: abi::FochStr::from_str("owned-buffer-test"),
						run_id: abi::FochStr::EMPTY,
						game_version: abi::FochStr::EMPTY,
						exe_sha256: abi::FochStr::EMPTY,
						exe_base: image.address.cast(),
						exe_size: IMAGE_SIZE,
						log: unused_log,
						report: unused_report,
						dir: unused_dir,
						config_json: unused_config,
						plugin_state: unused_plugin_state,
						find_pattern: unused_find,
						hook_create: unused_hook_create,
						hook_set_enabled: unused_hook_toggle,
						patch_create: create_patch,
						patch_set_enabled: toggle_patch,
					},
					state: Mutex::new(HostState::default()),
				})
			}
		}

		unsafe extern "C" fn unused_log(_: *const FochHostApi, _: i32, _: abi::FochStr) {}
		unsafe extern "C" fn unused_report(_: *const FochHostApi, _: *const abi::FochStatus) {}
		unsafe extern "C" fn unused_dir(_: *const FochHostApi, _: i32) -> abi::FochStr {
			abi::FochStr::EMPTY
		}
		unsafe extern "C" fn unused_config(_: *const FochHostApi) -> abi::FochStr {
			abi::FochStr::EMPTY
		}
		unsafe extern "C" fn unused_plugin_state(_: *const FochHostApi, _: abi::FochStr) -> i32 {
			0
		}
		unsafe extern "C" fn unused_find(
			_: *const FochHostApi,
			_: abi::FochStr,
			_: *mut *mut c_void,
		) -> i32 {
			abi::result::E_UNSUPPORTED
		}
		unsafe extern "C" fn unused_hook_create(
			_: *const FochHostApi,
			_: *mut c_void,
			_: *mut c_void,
			_: i32,
			_: *mut *const *const c_void,
			_: *mut *mut abi::FochHook,
		) -> i32 {
			abi::result::E_UNSUPPORTED
		}
		unsafe extern "C" fn unused_hook_toggle(
			_: *const FochHostApi,
			_: *mut abi::FochHook,
			_: i32,
		) -> i32 {
			abi::result::E_UNSUPPORTED
		}

		fn patch_handle(index: usize) -> *mut FochPatch {
			core::ptr::without_provenance_mut(index + 1)
		}

		unsafe extern "C" fn create_patch(
			host: *const FochHostApi,
			address: *mut c_void,
			bytes: *const u8,
			len: usize,
			output: *mut *mut FochPatch,
		) -> i32 {
			let context = unsafe { &*host.cast::<TestHost>() };
			if output.is_null() || bytes.is_null() || len != 4 {
				return abi::result::E_INVALID;
			}
			unsafe { *output = core::ptr::null_mut() };
			let base = context.api.exe_base as usize;
			if (address as usize) < base
				|| (address as usize)
					.checked_add(len)
					.is_none_or(|end| end > base + IMAGE_SIZE)
			{
				return abi::result::E_INVALID;
			}
			let Ok(mut state) = context.state.lock() else {
				return abi::result::E_INVALID;
			};
			let index = state.patches.len();
			state.calls.push(HostCall::Create(index));
			if state.fail_create == Some(index) {
				return abi::result::E_CONFLICT;
			}
			state.patches.push(TestPatch {
				address: address as usize,
				original: unsafe { core::slice::from_raw_parts(address.cast(), len) }.to_vec(),
				bytes: unsafe { core::slice::from_raw_parts(bytes, len) }.to_vec(),
				enabled: false,
			});
			// Handles are opaque indexes; no callback dereferences these values.
			unsafe { *output = patch_handle(index) };
			abi::result::OK
		}

		unsafe extern "C" fn toggle_patch(
			host: *const FochHostApi,
			patch: *mut FochPatch,
			enabled: i32,
		) -> i32 {
			let context = unsafe { &*host.cast::<TestHost>() };
			let Some(index) = (patch as usize).checked_sub(1) else {
				return abi::result::E_INVALID;
			};
			let Ok(mut state) = context.state.lock() else {
				return abi::result::E_INVALID;
			};
			if index >= state.patches.len() {
				return abi::result::E_INVALID;
			}
			let enabled = enabled != 0;
			state.calls.push(HostCall::Toggle(index, enabled));
			if (enabled && state.fail_enable == Some(index))
				|| (!enabled && state.fail_disable == Some(index))
			{
				return abi::result::E_MEMORY;
			}
			let patch = &mut state.patches[index];
			if patch.enabled == enabled {
				return abi::result::OK;
			}
			if enabled
				&& unsafe {
					core::slice::from_raw_parts(patch.address as *const u8, patch.original.len())
				} != patch.original
			{
				return abi::result::E_CONFLICT;
			}
			let bytes = if enabled {
				&patch.bytes
			} else {
				&patch.original
			};
			let address = patch.address as *mut c_void;
			let mut previous = 0;
			if unsafe { VirtualProtect(address, bytes.len(), 0x04, &mut previous) } == 0 {
				return abi::result::E_MEMORY;
			}
			unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), address.cast(), bytes.len()) };
			let mut discarded = 0;
			if unsafe { VirtualProtect(address, bytes.len(), previous, &mut discarded) } == 0 {
				return abi::result::E_MEMORY;
			}
			if unsafe { FlushInstructionCache(GetCurrentProcess(), address, bytes.len()) } == 0 {
				return abi::result::E_MEMORY;
			}
			patch.enabled = enabled;
			abi::result::OK
		}

		fn call_redirect<'a>(entry: usize, original: &'a [u8]) -> Redirect<'a> {
			Redirect {
				site_rva: entry + 4,
				expected_target_rva: ORIGINAL_TARGET,
				original,
				callback: replacement as *const c_void,
			}
		}

		#[test]
		fn actual_call_redirect_executes_the_dll_callback_through_a_near_thunk() {
			let image = OwnedImage::new();
			let original = image.snapshot(FIRST_ENTRY + 4, 5);
			assert_eq!(image.call(FIRST_ENTRY), ORIGINAL_RESULT);
			let host = TestHost::new(&image);
			unsafe { install(&host.api, &[call_redirect(FIRST_ENTRY, &original)]) }.unwrap();
			assert_eq!(image.call(FIRST_ENTRY), REPLACEMENT_RESULT);
			assert_ne!(image.snapshot(FIRST_ENTRY + 4, 5), original);
		}

		#[test]
		fn actual_lea_redirect_returns_a_callable_near_thunk() {
			let image = OwnedImage::new();
			let original = image.snapshot(LEA_ENTRY, 7);
			let old_pointer = image.pointer_from_lea();
			assert_eq!(
				old_pointer,
				unsafe { image.address.add(ORIGINAL_TARGET) }.cast()
			);
			let old_function: unsafe extern "C" fn() -> u32 =
				unsafe { core::mem::transmute(old_pointer) };
			assert_eq!(unsafe { old_function() }, ORIGINAL_RESULT);
			let host = TestHost::new(&image);
			let redirect = Redirect {
				site_rva: LEA_ENTRY,
				expected_target_rva: ORIGINAL_TARGET,
				original: &original,
				callback: replacement as *const c_void,
			};
			unsafe { install(&host.api, &[redirect]) }.unwrap();
			let pointer = image.pointer_from_lea();
			assert_ne!(pointer, old_pointer);
			let function: unsafe extern "C" fn() -> u32 = unsafe { core::mem::transmute(pointer) };
			assert_eq!(unsafe { function() }, REPLACEMENT_RESULT);
		}

		#[test]
		fn actual_enable_failure_restores_original_bytes_and_execution() {
			let image = OwnedImage::new();
			let original_image = image.snapshot(0, IMAGE_SIZE);
			let first = image.snapshot(FIRST_ENTRY + 4, 5);
			let second = image.snapshot(SECOND_ENTRY + 4, 5);
			let host = TestHost::new(&image);
			host.state.lock().unwrap().fail_enable = Some(1);
			let error = unsafe {
				install(
					&host.api,
					&[
						call_redirect(FIRST_ENTRY, &first),
						call_redirect(SECOND_ENTRY, &second),
					],
				)
			}
			.unwrap_err();
			assert_eq!(error.code, abi::result::E_MEMORY);
			assert_eq!(error.site_rva, SECOND_ENTRY + 4);
			assert!(!error.rollback_failed);
			assert_eq!(image.snapshot(0, IMAGE_SIZE), original_image);
			assert_eq!(image.call(FIRST_ENTRY), ORIGINAL_RESULT);
			assert_eq!(image.call(SECOND_ENTRY), ORIGINAL_RESULT);
			assert_eq!(
				host.state.lock().unwrap().calls,
				[
					HostCall::Create(0),
					HostCall::Create(1),
					HostCall::Toggle(0, true),
					HostCall::Toggle(1, true),
					HostCall::Toggle(0, false),
				]
			);
		}

		#[test]
		fn actual_registration_failure_never_changes_executable_bytes() {
			let image = OwnedImage::new();
			let original_image = image.snapshot(0, IMAGE_SIZE);
			let first = image.snapshot(FIRST_ENTRY + 4, 5);
			let second = image.snapshot(SECOND_ENTRY + 4, 5);
			let host = TestHost::new(&image);
			host.state.lock().unwrap().fail_create = Some(1);
			let error = unsafe {
				install(
					&host.api,
					&[
						call_redirect(FIRST_ENTRY, &first),
						call_redirect(SECOND_ENTRY, &second),
					],
				)
			}
			.unwrap_err();
			assert_eq!(error.code, abi::result::E_CONFLICT);
			assert_eq!(error.site_rva, SECOND_ENTRY + 4);
			assert!(!error.rollback_failed);
			assert_eq!(image.snapshot(0, IMAGE_SIZE), original_image);
			assert_eq!(image.call(FIRST_ENTRY), ORIGINAL_RESULT);
			assert_eq!(image.call(SECOND_ENTRY), ORIGINAL_RESULT);
			assert_eq!(
				host.state.lock().unwrap().calls,
				[HostCall::Create(0), HostCall::Create(1)]
			);
		}

		#[test]
		fn actual_validation_refuses_changed_bytes_or_target_before_registering() {
			let image = OwnedImage::new();
			let original_image = image.snapshot(0, IMAGE_SIZE);
			let call = image.snapshot(FIRST_ENTRY + 4, 5);
			let host = TestHost::new(&image);
			let mut redirect = call_redirect(FIRST_ENTRY, &call);
			redirect.expected_target_rva += 1;
			let target_error = unsafe { install(&host.api, &[redirect]) }.unwrap_err();
			assert_eq!(target_error.code, abi::result::E_UNSUPPORTED);
			// This fixture expects another valid CALL whose target differs by one.
			let mut changed = call;
			changed[1] += 1;
			let mut redirect = call_redirect(FIRST_ENTRY, &changed);
			redirect.expected_target_rva += 1;
			let bytes_error = unsafe { install(&host.api, &[redirect]) }.unwrap_err();
			assert_eq!(bytes_error.code, abi::result::E_CONFLICT);
			assert_eq!(image.snapshot(0, IMAGE_SIZE), original_image);
			assert_eq!(image.call(FIRST_ENTRY), ORIGINAL_RESULT);
			assert!(host.state.lock().unwrap().calls.is_empty());
		}

		#[test]
		fn actual_rollback_failure_retains_the_live_thunk_until_fixture_cleanup() {
			let image = OwnedImage::new();
			let original_image = image.snapshot(0, IMAGE_SIZE);
			let first = image.snapshot(FIRST_ENTRY + 4, 5);
			let second = image.snapshot(SECOND_ENTRY + 4, 5);
			let host = TestHost::new(&image);
			{
				let mut state = host.state.lock().unwrap();
				state.fail_enable = Some(1);
				state.fail_disable = Some(0);
			}
			let error = unsafe {
				install(
					&host.api,
					&[
						call_redirect(FIRST_ENTRY, &first),
						call_redirect(SECOND_ENTRY, &second),
					],
				)
			}
			.unwrap_err();
			assert_eq!(error.code, abi::result::E_MEMORY);
			assert!(error.rollback_failed);
			// install's temporary owners have dropped, but this patch remains live.
			assert_eq!(image.call(FIRST_ENTRY), REPLACEMENT_RESULT);
			assert_eq!(image.call(SECOND_ENTRY), ORIGINAL_RESULT);
			host.state.lock().unwrap().fail_disable = None;
			assert_eq!(
				unsafe { toggle_patch(&host.api, patch_handle(0), 0) },
				abi::result::OK
			);
			assert_eq!(image.snapshot(0, IMAGE_SIZE), original_image);
			assert_eq!(image.call(FIRST_ENTRY), ORIGINAL_RESULT);
		}
	}
}
