use crate::filesystem::{FileWatcher, FS};
use crate::utils::strip_quotes;
use log::warn;
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Not;
use std::path::{Path, PathBuf};
use std::process::Command;
use tree_sitter::{Node as TSNode, Range, Tree as TSTree};

pub struct ModEntry {
	pub name: String,
	pub path: PathBuf,
	pub version: String,
	pub supported_version: String,
	pub remote_file_id: String,
}

impl ModEntry {
	pub fn from_mod_record(path: &PathBuf) -> Self {
		todo!()
	}

	pub fn from_mod_descriptor(path: &PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
		let descriptor_text = std::fs::read_to_string(path)?;
		let mut parser = crate::parsing::TSParserWrapper::new();
		let _ = parser.parse(&descriptor_text);
		let nodes = parser
			.find_nodes(|node| node.kind() == "assignment")
			.expect("Failed to find assignment nodes");
		let hash_map = nodes
			.into_iter()
			.filter_map(|node| {
				let key = node.child_by_field_name("key")?;
				let key_name = key.utf8_text(&descriptor_text.as_ref()).ok()?;
				match key_name {
					"version" | "name" | "supported_version" | "remote_file_id" => {
						let value = node.child_by_field_name("value")?;
						let value_text = value.utf8_text(&descriptor_text.as_ref()).ok()?;
						Some((key_name.to_string(), value_text.to_string()))
					}
					_ => None,
				}
			})
			.collect::<HashMap<String, String>>();

		let name = strip_quotes(&hash_map["name"])?;
		let path = path
			.parent()
			.expect("Failed to get parent directory of descriptor file")
			.to_path_buf();
		let version = strip_quotes(&hash_map["version"])?;
		let supported_version = strip_quotes(&hash_map["supported_version"])?;
		let remote_file_id = strip_quotes(&hash_map["remote_file_id"])?;

		Ok(Self {
			name,
			path,
			version,
			supported_version,
			remote_file_id,
		})
	}
}

pub struct Mod {
	pub name: String,
	pub version: String,
	pub fw: Box<dyn FileWatcher>,
}

impl From<ModEntry> for Mod {
	fn from(entry: ModEntry) -> Self {
		Mod {
			name: entry.name,
			version: entry.version,
			fw: FS::new_file_watcher(entry.path).expect("Failed to create file watcher"),
		}
	}
}

impl Mod {
	pub fn find_conflict_files(
		&mut self,
		other: &mut Self,
	) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
		let file_snapshot = self.fw.file_snapshot()?;
		let other_file_snapshot = other.fw.file_snapshot()?;
		let mut conflicts = Vec::new();
		for (path, hash) in file_snapshot {
			if let Some(other_hash) = other_file_snapshot.get(&path) {
				let self_hash = match hash {
					Some(h) => h,
					None => self
						.fw
						.update_hash(&path)
						.map_err(|e| format!("Failed to get fingerprint for {:?}: {}", path, e))?,
				};
				let other_hash = match other_hash {
					Some(h) => h.clone(),
					None => other
						.fw
						.update_hash(&path)
						.map_err(|e| format!("Failed to get fingerprint for {:?}: {}", path, e))?,
				};
				if self_hash != other_hash {
					conflicts.push(path);
				}
			}
		}
		Ok(conflicts)
	}

	pub fn merge(&self, other: &Self) -> Self {
		todo!("Implement merge logic for mods")
	}
}

fn iter_tree(tree: &TSTree, src: &str) {
	let root = tree.root_node();
	let mut cursor = root.walk();
	let mut stack = vec![root];
	while let Some(node) = stack.pop() {
		println!("{:?}", node.utf8_text(src.as_ref()).unwrap());
		for child in node.children(&mut cursor) {
			stack.push(child);
		}
	}
}

// -------- 入口：顶层只看 assignment，同键再合并 --------

pub fn merge_root(
	a_root: TSNode,
	b_root: TSNode,
	merge_info: MergeInfo<'_>
) -> Result<String, Box<dyn std::error::Error>> {
	assert_eq!(a_root.kind(), "source_file");
	assert_eq!(b_root.kind(), "source_file");

	let a_map = top_level_assign_map(a_root, merge_info.a_text);
	let b_map = top_level_assign_map(b_root, merge_info.b_text);

	let mut keys = BTreeSet::new();
	keys.extend(a_map.keys().cloned());
	keys.extend(b_map.keys().cloned());

	let mut out = String::new();
	for k in keys {
		match (a_map.get(&k), b_map.get(&k)) {
			(Some(va), Some(vb)) => {
				let merged = merge_value_same_level(*va, *vb, &merge_info)?;
				// 简单渲染：k = <merged>
				out.push_str(&format!("{} = {}\n", k, merged));
			}
			(Some(va), None) => out.push_str(&format!("{} = {}\n", k, slice(merge_info.a_text, *va))),
			(None, Some(vb)) => out.push_str(&format!("{} = {}\n", k, slice(merge_info.b_text, *vb))),
			_ => {}
		}
	}
	Ok(out)
}

// -------- 顶层 assignment 抽取：key -> value_node --------

fn top_level_assign_map<'a>(root: TSNode<'a>, src: &str) -> BTreeMap<String, TSNode<'a>> {
	let mut map = BTreeMap::new();
	let mut c = root.walk();
	for n in root.children(&mut c) {
		let n = if n.kind() == "statement" {
			n.child(0).unwrap_or(n)
		} else {
			n
		};

		if n.kind() != "assignment" {
			continue;
		}
		if let (Some(k), Some(v)) = (n.child_by_field_name("key"), n.child_by_field_name("value")) {
			map.insert(slice(src, k).to_string(), v);
		}
	}
	map
}

fn context_lines<'a>(src: &'a str, conflict_begin_line: usize, conflict_end_line: usize) -> (Vec<(usize, &'a str)>, Vec<(usize, &'a str)>) {
	// 返回 [(行号, 行文本, 是否为当前行)]
	let mut pre_context = Vec::new();
	let mut post_context = Vec::new();
	let lines= src.split_terminator('\n').collect::<Vec<_>>();
	let start = conflict_begin_line.saturating_sub(5).max(1);
	let end = (conflict_end_line + 5).min(lines.len());
	for n in start..conflict_begin_line {
		let text = lines.get(n - 1).expect("line number out of range").to_owned();;
		pre_context.push((n, text));
	}
	for n in conflict_end_line+1..=end {
		let text = lines.get(n - 1).expect("line number out of range").to_owned();
		post_context.push((n, text));
	}
	(pre_context, post_context)
}

// -------- 合并“同键”的 value：array 并集；map 递归；标量冲突 --------

fn merge_value_same_level(
	va: TSNode,
	vb: TSNode,
	merge_info: &MergeInfo<'_>,
) -> Result<String, Box<dyn std::error::Error>> {
	match (va.kind(), vb.kind()) {
		// 列表层（Vec）：array vs array = 稳定并集（A 顺序 + 追加 B 未见项）
		("array", "array") => {
			let la = array_items(va, merge_info.a_text);
			let lb = array_items(vb, merge_info.b_text);
			println!("Merging arrays: {:?} and {:?}", la, lb);
			let mut seen = BTreeSet::<String>::new();
			let mut items = Vec::<String>::new();
			for s in la {
				if seen.insert(s.clone()) {
					items.push(s);
				}
			}
			for s in lb {
				if seen.insert(s.clone()) {
					items.push(s);
				}
			}
			Ok(render_array_inline(&items))
		}
		// 字典层（KV）：map vs map = 按键对齐，同键递归
		("map", "map") => {
			let ma = map_kv_once(va, merge_info.a_text);
			let mb = map_kv_once(vb, merge_info.b_text);
			let mut keys = BTreeSet::new();
			keys.extend(ma.keys().cloned());
			keys.extend(mb.keys().cloned());

			// 渲染为 { \n  k = v\n ... }
			let mut body = String::new();
			for k in keys {
				match (ma.get(&k), mb.get(&k)) {
					(Some(ka), Some(kb)) => {
						let merged = merge_value_same_level(*ka, *kb, merge_info)?;
						body.push_str(&format!("  {} = {}\n", k, merged));
					}
					(Some(ka), None) => {
						body.push_str(&format!("  {} = {}\n", k, slice(merge_info.a_text, *ka)))
					}
					(None, Some(kb)) => {
						body.push_str(&format!("  {} = {}\n", k, slice(merge_info.b_text, *kb)))
					}
					_ => {
						unreachable!("Both values should not be None for the same key: {}", k);
					}
				}
			}
			Ok(format!("{{\n{}}}", body))
		}
		// 相同 kind 的“标量/简单值”：文本规格化后相同算一致，否则冲突
		(ka, kb) if ka == kb => {
			let ta = normalize_scalar(slice(merge_info.a_text, va));
			let tb = normalize_scalar(slice(merge_info.b_text, vb));
			if ta == tb {
				Ok(slice(merge_info.a_text, va).to_string())
			} else {
				let conf = Conflict {
					path: merge_info.path,
					src: merge_info.a_text,
					ours: slice(merge_info.a_text, va),
					theirs: slice(merge_info.b_text, vb),
					range: va.range()
				};
				resolve_conflict(&conf, &merge_info.opt)
			}
		}
		// 不同 kind：类型冲突 → 冲突标记
		_ => {
			let conf = Conflict {
				path: merge_info.path,
				src: merge_info.a_text,
				ours: slice(merge_info.a_text, va),
				theirs: slice(merge_info.b_text, vb),
				range: va.range()
			};
			resolve_conflict(&conf, &merge_info.opt)
		}
	}
}

// -------- map/array 的结构化抽取（按你的 grammar） --------

// map: { statement* } ; 只看一层 assignment 键值
fn map_kv_once<'a>(map_node: TSNode<'a>, src: &str) -> BTreeMap<String, TSNode<'a>> {
	let mut out = BTreeMap::new();
	debug_assert_eq!(map_node.kind(), "map");
	let mut c = map_node.walk();
	for st in map_node.children(&mut c) {
		if st.kind() != "assignment" {
			continue;
		}
		if let (Some(k), Some(v)) = (
			st.child_by_field_name("key"),
			st.child_by_field_name("value"),
		) {
			out.insert(slice(src, k).to_string(), v);
		}
	}
	out
}

// array: { (simple_value|variable|variable_embedded_identifier)* }
fn array_items(arr_node: TSNode, src: &str) -> Vec<String> {
	debug_assert_eq!(arr_node.kind(), "array");
	let mut items = Vec::new();
	let mut c = arr_node.walk();
	for child in arr_node.children(&mut c) {
		match child.kind() {
			// array 内允许的元素都作为“一个 item”的文本
			"string"
			| "simple_value"
			| "number"
			| "boolean"
			| "identifier"
			| "variable"
			| "template_string"
			| "variable_embedded_identifier" => {
				items.push(slice(src, child).trim().to_string());
			}
			// 嵌套 array/map 一般不是 simple_value：若 grammar 允许，可递归或当作一个整体文本
			_ => {
				println!("Skipping non-simple value in array: {}", child.kind());
			}
		}
	}
	items
}

// -------- 渲染/规范化/切片 --------

fn render_array_inline(items: &[String]) -> String {
	if items.is_empty() {
		"{}".into()
	} else {
		format!("{{ {} }}", items.join(" "))
	}
}

fn render_conflict(a: &str, b: &str) -> String {
	format!("<<<<<<< A\n{}\n=======\n{}\n>>>>>>> B", a, b)
}

fn normalize_scalar(s: &str) -> String {
	// 去冗余空白，数字/标识符/布尔可借此“宽松相等”
	s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[inline]
fn slice<'a>(src: &'a str, n: TSNode<'a>) -> &'a str {
	let r = n.byte_range();
	&src[r.start..r.end]
}

#[derive(Clone, Debug)]
pub struct Conflict<'a> {
	pub path: &'a Path,   // 例如 "name" / "block/foo/name"
	pub src: &'a str,
	pub ours: &'a str,   // A侧文本
	pub theirs: &'a str, // B侧文本
	pub range: Range
}

#[derive(Clone, Copy, Debug)]
pub enum ConflictStrategy {
	Ask,
	Ours,
	Theirs,
	Abort,
}

#[derive(Clone, Debug)]
pub struct MergeOptions {
	pub strategy: ConflictStrategy,
	pub interactive: bool,
	pub editor: Option<String>,
}

impl Default for MergeOptions {
	fn default() -> Self {
		Self {
			strategy: ConflictStrategy::Ask,
			interactive: true,
			editor: None,
		}
	}
}

pub fn resolve_conflict(
	conf: &Conflict,
	opts: &MergeOptions,
) -> Result<String, Box<dyn std::error::Error>> {
	use ConflictStrategy::*;
	use dialoguer::{theme::ColorfulTheme, Select};
	let is_tty = atty::is(atty::Stream::Stdin) && atty::is(atty::Stream::Stdout);

	match (opts.strategy, opts.interactive && is_tty) {
		(Ours, _) => Ok(conf.ours.to_string()),
		(Theirs, _) => Ok(conf.theirs.to_string()),
		(Abort, _) => Err(format!("conflict at {} (abort)", conf.path.display()).into()),
		(Ask, false) => Err(format!("conflict at {} (non-interactive)", conf.path.display()).into()),
		(Ask, true) => {
			let theme = ColorfulTheme::default();

			let editor = opts
				.editor
				.clone()
				.or_else(|| std::env::var("VISUAL").ok())
				.or_else(|| std::env::var("EDITOR").ok())
				.unwrap_or_else(|| {
					warn!("No $VISUAL or $EDITOR specified, using default");
					if cfg!(windows) {
						"notepad".into()
					} else {
						"nano".into()
					}
				});

			let items = &[
				format!("Use ours   ← {}", ellip(&conf.ours, 60)),
				format!("Use theirs ← {}", ellip(&conf.theirs, 60)),
				format!("Edit manually in <{}>", editor).into(),
				"Abort".into(),
			];

			let pre_context = &conf.src[..conf.range.start_byte].split_terminator('\n').collect::<Vec<_>>();
			let pre_context = &pre_context[pre_context.len().saturating_sub(10)..];
			let pre_context = pre_context.join("\n");
			let post_context = &conf.src[conf.range.end_byte..];
			let post_context = &post_context[..post_context.len().saturating_sub(10)];
			let post_context = post_context.split_terminator('\n').collect::<Vec<_>>();
			let post_context = post_context.join("\n");

			let mut to_edit = String::new();

			to_edit.push_str(&format!("{}", pre_context));

			to_edit.push_str("\n<<< BEGIN EDIT >>>\n");
			to_edit.push_str(&format!("<<<<<<< ours\n{}\n=======\n{}\n>>>>>>> theirs", conf.ours, conf.theirs));
			to_edit.push_str("\n<<< END EDIT >>>\n");


			to_edit.push_str(&format!("{}", post_context));


			let sel = Select::with_theme(&theme)
				.with_prompt(format!("Conflict at {}\n{}", conf.path.display(), to_edit))
				.items(items)
				.default(2)
				.interact()?;

			match sel {
				0 => Ok(conf.ours.to_string()),
				1 => Ok(conf.theirs.to_string()),
				2 => edit_in_editor(editor, &to_edit),
				_ => Err("aborted by user".into()),
			}
		}
	}


}


fn spawn_editor(editor_spec: &str, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
	// 将 "code --wait" → ["code","--wait"]
	let mut parts = shell_words::split(editor_spec).expect("invalid editor spec");
	let prog = parts
		.get(0)
		.expect("editor spec must contain at least one program name");
	let args = &parts[1..];

	let exit = Command::new(prog)
		.args(args)
		.arg(path)
		.status()
		.expect("failed to spawn editor");
	exit.success()
		.then_some(())
		.ok_or_else(|| {
			format!(
				"Editor '{}' failed with exit code: {}",
				editor_spec, exit.code().unwrap_or(-1)
			)
			.into()
		})
}

fn edit_in_editor(
	editor: String,
	to_edit: &String,
) -> Result<String, Box<dyn std::error::Error>> {
	use std::{fs, io::Write };
	let mut tmp = tempfile::NamedTempFile::new().expect("Failed to create temp");


	writeln!(
		tmp,
		"{}",to_edit
	)?;
	tmp.flush()?;


	println!("Edit in editor: {}", editor);
	spawn_editor(editor.as_str(), tmp.path())?;

	let contents = fs::read_to_string(tmp.path())?;
	let edit_block = extract_edit_block(&contents).expect("Failed to extract edit block");

	Ok(edit_block.to_string())
}

fn extract_edit_block(all: &str) -> Option<&str> {
	let begin = "\n<<< BEGIN EDIT >>>\n";
	let end = "\n<<< END EDIT >>>\n";
	let start = all.find(begin)? + begin.len();
	let rest = &all[start..];
	let endpos = rest.find(end)?;
	Some(&rest[..endpos])
}

fn ellip(s: &str, n: usize) -> String {
	if s.len() <= n {
		s.into()
	} else {
		format!("{}…", &s[..n])
	}
}

pub struct MergeInfo<'a> {
	pub path: &'a Path,
	pub a_text: &'a str,
	pub b_text: &'a str,
	pub opt: MergeOptions,
}


fn byte_to_line(locs: &[usize], byte: usize) -> usize {
	// 二分找 <= byte 的最大行起始
	match locs.binary_search(&byte) {
		Ok(idx) => idx + 1,           // 行号 1-based
		Err(idx) => idx.max(1),       // 插入点 → 上一行
	}
}

#[cfg(test)]
mod tests {
	use super::*;
use crate::get_corpus_path;
use crate::parsing::TSParserWrapper;

	#[test]
	fn test_mod_entry_from_mod_descriptor() {
		let path = get_corpus_path().join("defines").join("descriptor.mod");
		let entry = ModEntry::from_mod_descriptor(&path).unwrap();
		assert_eq!(entry.name, "defines");
		assert_eq!(entry.version, "0.0.1");
		assert_eq!(entry.supported_version, "1.34.4");
	}

	#[test]
	fn test_find_conflict_files() {
		let path1 = get_corpus_path().join("defines").join("descriptor.mod");
		let path2 = get_corpus_path()
			.join("control_military_access")
			.join("descriptor.mod");
		let mut mod1 = Mod::from(ModEntry::from_mod_descriptor(&path1).unwrap());
		let mut mod2 = Mod::from(ModEntry::from_mod_descriptor(&path2).unwrap());
		println!("Mod1: {:?}, Mod2: {:?}", mod1.name, mod2.name);
		mod1.fw.collect_files().unwrap();
		mod2.fw.collect_files().unwrap();

		let conflicts = mod1.find_conflict_files(&mut mod2).unwrap();
		let files1 = mod1.fw.file_snapshot().unwrap();
		let files2 = mod2.fw.file_snapshot().unwrap();
		println!("Files in mod1: {:?}", files1);
		println!("Files in mod2: {:?}", files2);
		assert!(conflicts.len() == 1, "Expected conflicts of mod descriptor");
	}
	#[test]
	fn test_merge_tree() {
		let path1 = get_corpus_path().join("defines").join("descriptor.mod");
		let path2 = get_corpus_path()
			.join("control_military_access")
			.join("descriptor.mod");

		let mut parser_1 = TSParserWrapper::new();
		let mut parser_2 = TSParserWrapper::new();

		parser_1.parse(&std::fs::read_to_string(&path1).unwrap());
		parser_2.parse(&std::fs::read_to_string(&path2).unwrap());

		let text_1 = parser_1.text().expect("Failed to get text from parser 1");
		let text_2 = parser_2.text().expect("Failed to get text from parser 2");
		let tree_1 = parser_1.tree().expect("Failed to get tree from parser 1");
		let tree_2 = parser_2.tree().expect("Failed to get tree from parser 2");

		let merge_info = MergeInfo {
			path: path1.strip_prefix(get_corpus_path().join("defines")).expect("Failed to get path"),
			a_text: text_1,
			b_text: text_2,
			opt: MergeOptions::default(),
		};

		let res = merge_root(tree_1.root_node(), tree_2.root_node(),merge_info)
			.expect("Failed to merge trees");

		println!("res: {}", res);
	}
}
