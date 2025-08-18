pub fn strip_quotes(s: &str) -> Result<String, &str> {
	if s.starts_with('"') && s.ends_with('"') {
		Ok(s[1..s.len() - 1].to_string())
	} else {
		Err("String does not start and end with quotes")
	}
}
