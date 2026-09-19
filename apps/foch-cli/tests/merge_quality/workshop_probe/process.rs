use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::ProbeResult;

#[derive(Debug, Serialize)]
pub(super) struct ProcessResult {
	pub code: Option<i32>,
	pub timed_out: bool,
	pub elapsed_ms: u128,
}

impl ProcessResult {
	pub(super) fn failure(&self, stage: &str, dir: &Path) -> Option<String> {
		let reason = if self.timed_out {
			format!(
				"timed out after {:.1} seconds",
				self.elapsed_ms as f64 / 1000.0
			)
		} else if self.code != Some(0) {
			format!("failed with exit code {:?}", self.code)
		} else {
			return None;
		};
		let stderr: PathBuf = dir.join(format!("{stage}.stderr.log"));
		Some(format!(
			"{stage} {reason}; inspect {} and {}\nlast stderr lines:\n{}",
			stderr.display(),
			dir.join(format!("{stage}.stdout.log")).display(),
			log_tail(&stderr, 20)
		))
	}
}

/// The last `lines` lines of a log. A failure message carries them because
/// the log itself may live in a temporary directory that is gone by the time
/// anyone reads the message, as on CI.
fn log_tail(path: &Path, lines: usize) -> String {
	let log: String = std::fs::read_to_string(path).unwrap_or_default();
	let tail: Vec<&str> = log.lines().rev().take(lines).collect();
	tail.into_iter().rev().collect::<Vec<&str>>().join("\n")
}

pub(super) fn run(
	command: &mut Command,
	dir: &Path,
	stage: &str,
	timeout: Duration,
) -> ProbeResult<ProcessResult> {
	let log = |stream: &str| {
		OpenOptions::new()
			.write(true)
			.create_new(true)
			.open(dir.join(format!("{stage}.{stream}.log")))
	};
	command
		.stdin(Stdio::null())
		.stdout(log("stdout")?)
		.stderr(log("stderr")?);
	#[cfg(unix)]
	{
		use std::os::unix::process::CommandExt;
		command.process_group(0);
	}
	let started = Instant::now();
	let mut child = command.spawn()?;
	eprintln!(
		"[workshop-probe] {stage}: start pid={}; logs in {}",
		child.id(),
		dir.display()
	);
	let mut next_progress = Duration::from_secs(10);
	loop {
		if let Some(status) = child.try_wait()? {
			eprintln!(
				"[workshop-probe] {stage}: {status}, elapsed {:?}",
				started.elapsed()
			);
			return Ok(ProcessResult {
				code: status.code(),
				timed_out: false,
				elapsed_ms: started.elapsed().as_millis(),
			});
		}
		if started.elapsed() >= timeout {
			#[cfg(unix)]
			// The child owns this process group, including SteamCMD shell children.
			unsafe {
				libc::kill(-(child.id() as i32), libc::SIGKILL);
			}
			#[cfg(not(unix))]
			child.kill()?;
			child.wait()?;
			return Ok(ProcessResult {
				code: None,
				timed_out: true,
				elapsed_ms: started.elapsed().as_millis(),
			});
		}
		if started.elapsed() >= next_progress {
			match latest_log_line(&dir.join(format!("{stage}.stderr.log"))) {
				Ok(Some(line)) => eprintln!(
					"[workshop-probe] {stage}: elapsed {:?}; {line}",
					started.elapsed()
				),
				Ok(None) => eprintln!(
					"[workshop-probe] {stage}: elapsed {:?}; no stderr progress yet",
					started.elapsed()
				),
				Err(error) => eprintln!(
					"[workshop-probe] {stage}: elapsed {:?}; could not read progress: {error}",
					started.elapsed()
				),
			}
			next_progress += Duration::from_secs(10);
		}
		thread::sleep(Duration::from_millis(100));
	}
}

fn latest_log_line(path: &Path) -> io::Result<Option<String>> {
	const MAX_BYTES: u64 = 8192;
	let mut file = File::open(path)?;
	let offset = file.metadata()?.len().saturating_sub(MAX_BYTES);
	file.seek(SeekFrom::Start(offset))?;
	let mut bytes = Vec::new();
	file.take(MAX_BYTES).read_to_end(&mut bytes)?;
	// A window may start inside a UTF-8 character or a log line. Ignore that
	// partial first line; logging must not interrupt an otherwise healthy merge.
	let text = String::from_utf8_lossy(&bytes);
	let complete = if offset > 0 {
		text.split_once('\n').map_or("", |(_, tail)| tail)
	} else {
		&text
	};
	Ok(complete
		.lines()
		.rev()
		.find(|line| !line.trim().is_empty())
		.map(str::to_owned))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn progress_reads_latest_line_from_a_bounded_utf8_log_tail() {
		let root = tempfile::tempdir().unwrap();
		let log = root.path().join("merge.stderr.log");
		std::fs::write(&log, "").unwrap();
		assert_eq!(latest_log_line(&log).unwrap(), None);
		let line = "[merge] structural file: start history/advisors/00_converter_advisors.txt";
		std::fs::write(&log, format!("{}\n{line}\n\n", "注释".repeat(5000))).unwrap();
		assert_eq!(latest_log_line(&log).unwrap().as_deref(), Some(line));
	}

	#[test]
	fn process_timeout_is_bounded_and_keeps_logs() {
		let root = tempfile::tempdir().unwrap();
		let mut command = Command::new(std::env::current_exe().unwrap());
		command
			.args([
				"workshop_probe::process::tests::sleeping_child",
				"--exact",
				"--ignored",
			])
			.env("FOCH_PROBE_SLEEP_CHILD", "1");
		let result = run(
			&mut command,
			root.path(),
			"timeout",
			Duration::from_millis(100),
		)
		.unwrap();
		assert!(result.timed_out);
		assert!(result.elapsed_ms < 5000);
		assert!(root.path().join("timeout.stdout.log").is_file());
		let message = result.failure("timeout", root.path()).unwrap();
		assert!(message.contains("timed out after"));
		assert!(message.contains("timeout.stderr.log"));
	}

	#[test]
	#[ignore = "subprocess fixture for process_timeout_is_bounded_and_keeps_logs"]
	fn sleeping_child() {
		if std::env::var("FOCH_PROBE_SLEEP_CHILD").as_deref() == Ok("1") {
			thread::sleep(Duration::from_secs(10));
		}
	}
}
