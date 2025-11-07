use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct ModDescriptor {
	name: String,
	path: String, // "mod/ugc_12345/"

	#[serde(default)] // 如果 .mod 文件里没有 "dependencies" 字段，就默认为空 Vec
	dependencies: Vec<String>,

	// 有些 .mod 文件没有 version，所以用 Option
	version: Option<String>,

	// Steam Workshop ID
	remote_file_id: Option<String>,
}

impl<T: AsRef<Path>> TryFrom<T> for ModDescriptor {
    // 关键：将我们的自定义错误指定为 Trait 的关联类型
    type Error = ModParseError;

    // `value` 的类型是泛型 T (AsRef<Path>)
    fn try_from(value: T) -> Result<Self, Self::Error> {

        // 1. 用 .as_ref() 把它变成一个 &Path
        let path = value.as_ref();

        // 2. 读取字节（和上次一样，防止UTF-8错误）
        let data = std::fs::read(path).map_err(|e| ModParseError::Io {
            path: path.to_path_buf(),
            source: e
        })?;

        // 3. Jomini 解析
        let tape = TextTape::from_slice(&data).map_err(|e| ModParseError::Parse {
            path: path.to_path_buf(),
            source: e.into(),
        })?;

        let reader = tape.windows_1252_reader();

        let descriptor = Jomini::from_reader(reader)
            .deserialize()
            .map_err(|e| ModParseError::Parse {
                path: path.to_path_buf(),
                source: e,
            })?;

        Ok(descriptor)
    }
}
