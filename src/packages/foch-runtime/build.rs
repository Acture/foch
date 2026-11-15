fn main() {
	println!("cargo:rerun-if-changed=version.def");
	if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
		&& std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64")
	{
		let definition = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
			.join("version.def");
		if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
			println!("cargo:rustc-cdylib-link-arg=/DEF:{}", definition.display());
		} else {
			println!("cargo:rustc-cdylib-link-arg={}", definition.display());
		}
	}
}
