pub mod openrouter;

use std::path::Path;
use std::fs;

pub fn load_dotenv(dotenv_path: Option<&Path>) {
	let path = dotenv_path
		.map(|p| p.to_path_buf())
		.unwrap_or_else(|| Path::new(".env").to_path_buf());
	if !path.exists() {
		return;
	}
	if let Ok(text) = fs::read_to_string(&path) {
		for raw_line in text.split_terminator('\n') {
			let line = raw_line.trim();
			if line.is_empty() || line.starts_with('#') || !line.contains('=') {
				continue;
			}
			let mut parts = line.splitn(2, '=');
			if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
				let key: &str = k.trim();
				let value = v.trim().trim_matches('"').trim_matches('\'');
				if !key.is_empty() && std::env::var_os(key).is_none() {
					std::env::set_var(key, value);
				}
			}
		}
	}
}
