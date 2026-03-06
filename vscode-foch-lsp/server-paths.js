const path = require('path');

function serverExecutableName(platform = process.platform) {
	return platform === 'win32' ? 'foch_lsp.exe' : 'foch_lsp';
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

module.exports = {
	bundledServerFolder,
	bundledServerPath,
	serverExecutableName
};
