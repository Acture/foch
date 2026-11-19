//! Standalone slider panel for tribute-adjust.
//!
//! It does not inject into eu4.exe and does not hook DirectX. Pressing Save
//! writes `tribute-config.json`; the plugin reloads that file while the game
//! is running.

use foch_eu4_tribute::{SliderModel, SubjectTypeGraph, TributeConfig};
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
	data_dir: PathBuf,
	roots: Vec<PathBuf>,
	dump: bool,
}

fn main() -> ExitCode {
	let args = match parse_args(env::args().skip(1)) {
		Ok(args) => args,
		Err(message) => {
			eprintln!("{message}");
			eprintln!(
				"usage: tribute-panel --data-dir DIR [--root GAME_OR_MOD]... [--dump]\n\
				 roots are the game directory first, then mods in playset order"
			);
			return ExitCode::from(2);
		}
	};
	let graph = match SubjectTypeGraph::load_playset(&args.roots) {
		Ok(graph) => graph,
		Err(error) => {
			eprintln!("subject types: {error}");
			return ExitCode::from(1);
		}
	};
	let config = TributeConfig::default().overlay_data_file(&args.data_dir);
	let model = SliderModel::from_parts(&graph, &config);
	if args.dump {
		dump(&model);
		return ExitCode::SUCCESS;
	}
	#[cfg(windows)]
	{
		if let Err(error) = window::run(model, graph, args.data_dir) {
			eprintln!("{error}");
			return ExitCode::from(1);
		}
		ExitCode::SUCCESS
	}
	#[cfg(not(windows))]
	{
		let _ = (model, graph);
		eprintln!("tribute-panel's window is a Windows tool; pass --dump to print the tree");
		ExitCode::from(1)
	}
}

fn dump(model: &SliderModel) {
	println!("enabled={} global={}", model.enabled, model.global_percent);
	for row in &model.types {
		let explicit = row
			.explicit_percent
			.map(|percent| percent.to_string())
			.unwrap_or_else(|| "-".to_string());
		println!(
			"{:width$}{} effective={} explicit={explicit}",
			"",
			row.key,
			row.effective_percent,
			width = row.depth as usize * 2
		);
	}
	for tag in &model.tags {
		println!("tag {} {}", tag.tag, tag.percent);
	}
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
	let mut data_dir = None;
	let mut roots = Vec::new();
	let mut dump = false;
	let mut args = args.peekable();
	while let Some(arg) = args.next() {
		match arg.as_str() {
			"--dump" => dump = true,
			"--data-dir" => {
				data_dir = Some(PathBuf::from(
					args.next().ok_or("--data-dir needs a directory")?,
				));
			}
			"--root" => {
				roots.push(PathBuf::from(
					args.next().ok_or("--root needs a directory")?,
				));
			}
			other => return Err(format!("unknown argument {other}")),
		}
	}
	Ok(Args {
		data_dir: data_dir.ok_or("--data-dir is required")?,
		roots,
		dump,
	})
}

fn wide(text: &str) -> Vec<u16> {
	text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
mod window {
	use super::{SliderModel, SubjectTypeGraph, wide};
	use std::path::PathBuf;
	use windows_sys::Win32::Foundation::{HWND, POINT};
	use windows_sys::Win32::Graphics::Gdi::HBRUSH;
	use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
	use windows_sys::Win32::UI::Controls::{
		ICC_BAR_CLASSES, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx,
		TBM_SETPOS, TBM_SETRANGEMAX, TBM_SETRANGEMIN, TRACKBAR_CLASSW,
	};
	use windows_sys::Win32::UI::WindowsAndMessaging::{
		BS_AUTOCHECKBOX, BS_PUSHBUTTON, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW,
		DispatchMessageW, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, LB_ADDSTRING,
		LB_GETCURSEL, LB_SETCURSEL, LBN_SELCHANGE, LBS_NOTIFY, MSG, PostQuitMessage,
		RegisterClassW, SW_SHOWNORMAL, SendMessageW, SetWindowLongPtrW, SetWindowTextW, ShowWindow,
		TranslateMessage, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_HSCROLL, WNDCLASSW, WS_BORDER,
		WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
	};

	const TBM_GETPOS: u32 = 0x0400;

	const BM_GETCHECK: u32 = 0x00F0;
	const BM_SETCHECK: u32 = 0x00F1;

	struct Ui {
		model: SliderModel,
		graph: SubjectTypeGraph,
		data_dir: PathBuf,
		list: HWND,
		global: HWND,
		kind: HWND,
		enabled: HWND,
		save: HWND,
		status: HWND,
	}

	pub fn run(
		model: SliderModel,
		graph: SubjectTypeGraph,
		data_dir: PathBuf,
	) -> Result<(), String> {
		let ui = Box::new(Ui {
			model,
			graph,
			data_dir,
			list: std::ptr::null_mut(),
			global: std::ptr::null_mut(),
			kind: std::ptr::null_mut(),
			enabled: std::ptr::null_mut(),
			save: std::ptr::null_mut(),
			status: std::ptr::null_mut(),
		});
		let controls = INITCOMMONCONTROLSEX {
			dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
			dwICC: ICC_BAR_CLASSES | ICC_STANDARD_CLASSES,
		};
		// SAFETY: comctl32 copies the struct before returning.
		unsafe { InitCommonControlsEx(&controls) };
		let class = wide("FochTributePanel");
		let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
		let wndclass = WNDCLASSW {
			style: 0,
			lpfnWndProc: Some(wnd_proc),
			cbClsExtra: 0,
			cbWndExtra: 0,
			hInstance: instance,
			hIcon: std::ptr::null_mut(),
			hCursor: std::ptr::null_mut(),
			hbrBackground: 16 as HBRUSH,
			lpszMenuName: std::ptr::null(),
			lpszClassName: class.as_ptr(),
		};
		if unsafe { RegisterClassW(&wndclass) } == 0 {
			return Err("RegisterClassW failed".into());
		}
		let title = wide("Tribute Adjust");
		let hwnd = unsafe {
			CreateWindowExW(
				0,
				class.as_ptr(),
				title.as_ptr(),
				WS_OVERLAPPEDWINDOW | WS_VISIBLE,
				CW_USEDEFAULT,
				CW_USEDEFAULT,
				560,
				420,
				std::ptr::null_mut(),
				std::ptr::null_mut(),
				instance,
				Box::into_raw(ui).cast(),
			)
		};
		if hwnd.is_null() {
			return Err("CreateWindowExW failed".into());
		}
		unsafe {
			let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
			let mut message = MSG {
				hwnd: std::ptr::null_mut(),
				message: 0,
				wParam: 0,
				lParam: 0,
				time: 0,
				pt: POINT { x: 0, y: 0 },
			};
			while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
				let _ = TranslateMessage(&message);
				DispatchMessageW(&message);
			}
		}
		Ok(())
	}

	unsafe extern "system" fn wnd_proc(
		hwnd: HWND,
		message: u32,
		wparam: usize,
		lparam: isize,
	) -> isize {
		match message {
			WM_CREATE => {
				let create =
					lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
				let ui = unsafe { &mut *((*create).lpCreateParams.cast::<Ui>()) };
				unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, ui as *mut Ui as isize) };
				build_controls(hwnd, ui);
				0
			}
			WM_HSCROLL => {
				on_scroll(hwnd);
				0
			}
			WM_COMMAND => {
				on_command(hwnd, (wparam >> 16) as u16, lparam as HWND);
				0
			}
			WM_DESTROY => {
				let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
				if raw != 0 {
					unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
					drop(unsafe { Box::from_raw(raw as *mut Ui) });
				}
				unsafe { PostQuitMessage(0) };
				0
			}
			_ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
		}
	}

	fn build_controls(parent: HWND, ui: &mut Ui) {
		ui.global = trackbar(parent, 16, 16, 220);
		set_slider(ui.global, ui.model.global_percent);
		text_child(
			parent,
			"STATIC",
			"Global 0–200",
			248,
			16,
			200,
			24,
			WS_CHILD | WS_VISIBLE,
		);
		ui.enabled = text_child(
			parent,
			"BUTTON",
			"Enabled",
			248,
			40,
			120,
			28,
			WS_CHILD | WS_VISIBLE | BS_AUTOCHECKBOX as u32,
		);
		if ui.model.enabled {
			unsafe { SendMessageW(ui.enabled, BM_SETCHECK, 1, 0) };
		}
		ui.list = text_child(
			parent,
			"LISTBOX",
			"",
			16,
			72,
			250,
			220,
			WS_CHILD | WS_VISIBLE | WS_BORDER | WS_VSCROLL | LBS_NOTIFY as u32,
		);
		fill_list(ui);
		ui.kind = trackbar(parent, 280, 72, 220);
		text_child(
			parent,
			"STATIC",
			"Selected type",
			280,
			104,
			200,
			24,
			WS_CHILD | WS_VISIBLE,
		);
		ui.save = text_child(
			parent,
			"BUTTON",
			"Save",
			280,
			260,
			120,
			32,
			WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
		);
		ui.status = text_child(
			parent,
			"STATIC",
			"Save writes tribute-config.json",
			16,
			310,
			500,
			24,
			WS_CHILD | WS_VISIBLE,
		);
	}

	fn fill_list(ui: &Ui) {
		let selected = unsafe { SendMessageW(ui.list, LB_GETCURSEL, 0, 0) };
		unsafe { SendMessageW(ui.list, 0x0184, 0, 0) }; // LB_RESETCONTENT
		for row in &ui.model.types {
			let text = wide(&format!(
				"{:width$}{}  {}",
				"",
				row.key,
				row.effective_percent,
				width = row.depth as usize * 2
			));
			unsafe { SendMessageW(ui.list, LB_ADDSTRING, 0, text.as_ptr() as isize) };
		}
		if selected >= 0 {
			unsafe { SendMessageW(ui.list, LB_SETCURSEL, selected as usize, 0) };
		}
	}

	fn on_scroll(hwnd: HWND) {
		let Some(ui) = ui_mut(hwnd) else {
			return;
		};
		ui.model.set_global(slider_pos(ui.global), &ui.graph);
		if let Some(key) = selected_key(ui) {
			let percent = slider_pos(ui.kind);
			ui.model.set_type_percent(&key, percent, &ui.graph);
		}
		fill_list(ui);
		let _ = ui.save;
	}

	fn on_command(hwnd: HWND, code: u16, from: HWND) {
		let Some(ui) = ui_mut(hwnd) else {
			return;
		};
		if u32::from(code) == LBN_SELCHANGE
			&& from == ui.list
			&& let Some(key) = selected_key(ui)
		{
			let percent = ui
				.model
				.types
				.iter()
				.find(|row| row.key == key)
				.map(|row| row.effective_percent)
				.unwrap_or(ui.model.global_percent);
			set_slider(ui.kind, percent);
		}
		if from == ui.save {
			ui.model
				.set_enabled(unsafe { SendMessageW(ui.enabled, BM_GETCHECK, 0, 0) } == 1);
			let status = match ui.model.save(&ui.data_dir) {
				Ok(()) => "Saved. The plugin reloads within 250 ms.".to_string(),
				Err(error) => format!("Save failed: {error}"),
			};
			let status = wide(&status);
			unsafe { SetWindowTextW(ui.status, status.as_ptr()) };
		}
	}

	fn selected_key(ui: &Ui) -> Option<String> {
		let index = unsafe { SendMessageW(ui.list, LB_GETCURSEL, 0, 0) };
		if index < 0 {
			return None;
		}
		ui.model
			.types
			.get(index as usize)
			.map(|row| row.key.clone())
	}

	fn ui_mut(hwnd: HWND) -> Option<&'static mut Ui> {
		let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
		if raw == 0 {
			return None;
		}
		Some(unsafe { &mut *(raw as *mut Ui) })
	}

	fn trackbar(parent: HWND, x: i32, y: i32, width: i32) -> HWND {
		let blank = wide("");
		let hwnd = unsafe {
			CreateWindowExW(
				0,
				TRACKBAR_CLASSW,
				blank.as_ptr(),
				WS_CHILD | WS_VISIBLE,
				x,
				y,
				width,
				28,
				parent,
				std::ptr::null_mut(),
				GetModuleHandleW(std::ptr::null()),
				std::ptr::null_mut(),
			)
		};
		unsafe {
			SendMessageW(hwnd, TBM_SETRANGEMIN, 0, 0);
			SendMessageW(hwnd, TBM_SETRANGEMAX, 0, 200);
		}
		hwnd
	}

	fn set_slider(hwnd: HWND, percent: i64) {
		unsafe { SendMessageW(hwnd, TBM_SETPOS, 1, percent.clamp(0, 200) as isize) };
	}

	fn slider_pos(hwnd: HWND) -> i64 {
		unsafe { SendMessageW(hwnd, TBM_GETPOS, 0, 0) as i64 }
	}

	#[allow(clippy::too_many_arguments)]
	fn text_child(
		parent: HWND,
		class: &str,
		text: &str,
		x: i32,
		y: i32,
		width: i32,
		height: i32,
		style: u32,
	) -> HWND {
		let class = wide(class);
		let text = wide(text);
		unsafe {
			CreateWindowExW(
				0,
				class.as_ptr(),
				text.as_ptr(),
				style,
				x,
				y,
				width,
				height,
				parent,
				std::ptr::null_mut(),
				GetModuleHandleW(std::ptr::null()),
				std::ptr::null_mut(),
			)
		}
	}
}
