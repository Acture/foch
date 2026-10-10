//! Byte patterns such as `48 8B ?? 05 ?` and their unique match.

#[derive(Debug, Eq, PartialEq)]
pub struct Pattern(Vec<Option<u8>>);

impl Pattern {
	pub fn parse(text: &str) -> Option<Self> {
		let mut bytes = Vec::new();
		for token in text.split_ascii_whitespace() {
			if token.chars().all(|c| c == '?') && token.len() <= 2 {
				bytes.push(None);
			} else if token.len() == 2 {
				bytes.push(Some(u8::from_str_radix(token, 16).ok()?));
			} else {
				return None;
			}
		}
		// A pattern made only of wildcards matches everywhere.
		if bytes.iter().all(Option::is_none) {
			return None;
		}
		Some(Self(bytes))
	}

	fn matches_at(&self, haystack: &[u8], at: usize) -> bool {
		self.0
			.iter()
			.zip(&haystack[at..])
			.all(|(want, have)| want.is_none_or(|byte| byte == *have))
	}
}

#[derive(Debug, Eq, PartialEq)]
pub enum Found {
	Unique(usize),
	None,
	Ambiguous,
}

/// The offset of the only match of `pattern` in `haystack`.
pub fn find_unique(pattern: &Pattern, haystack: &[u8]) -> Found {
	let len = pattern.0.len();
	if len > haystack.len() {
		return Found::None;
	}
	let mut found = None;
	for at in 0..=haystack.len() - len {
		if pattern.matches_at(haystack, at) {
			if found.is_some() {
				return Found::Ambiguous;
			}
			found = Some(at);
		}
	}
	found.map_or(Found::None, Found::Unique)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_hex_and_wildcards() {
		let pattern = Pattern::parse("48 8b ?? 05 ?").unwrap();
		assert_eq!(
			pattern,
			Pattern(vec![Some(0x48), Some(0x8b), None, Some(0x05), None])
		);
		assert!(Pattern::parse("48 8").is_none());
		assert!(Pattern::parse("?? ??").is_none(), "wildcards only");
		assert!(Pattern::parse("").is_none());
	}

	#[test]
	fn finds_only_unique_matches() {
		let haystack = [0x90, 0x48, 0x8b, 0x01, 0x05, 0x90, 0x48, 0x8b, 0x02, 0x06];
		let unique = Pattern::parse("48 8B ?? 05").unwrap();
		assert_eq!(find_unique(&unique, &haystack), Found::Unique(1));
		let twice = Pattern::parse("48 8B").unwrap();
		assert_eq!(find_unique(&twice, &haystack), Found::Ambiguous);
		let absent = Pattern::parse("CC CC").unwrap();
		assert_eq!(find_unique(&absent, &haystack), Found::None);
	}
}
