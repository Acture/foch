const path = require('path');

function serverExecutableName(platform = process.platform) {
	return platform === 'win32' ? 'foch.exe' : 'foch';
}

function bundledServerFolder(platform = process.platform, arch = process.arch) {
	return `${platform}-${arch}`;
}

function bundledServerPath(extensionRoot, platform = process.platform, arch = process.arch) {
	return path.join(
		extensionRoot,
		'bin',
		bundledServerFolder(platform, arch),
		serverExecutableName(platform)
	);
}

/// Args appended after the server binary name when launching it as an LSP
/// server. The merged `foch` binary dispatches LSP via the `lsp` subcommand.
function bundledServerArgs() {
	return ['lsp'];
}

function vsceTarget(platform = process.platform, arch = process.arch) {
	const key = `${platform}-${arch}`;
	switch (key) {
		case 'darwin-arm64':
		case 'darwin-x64':
		case 'linux-arm64':
		case 'linux-x64':
		case 'win32-arm64':
		case 'win32-x64':
			return key;
		default:
			throw new Error(`unsupported VS Code target platform: ${key}`);
	}
}

module.exports = {
	bundledServerArgs,
	bundledServerFolder,
	bundledServerPath,
	serverExecutableName,
	vsceTarget
};
