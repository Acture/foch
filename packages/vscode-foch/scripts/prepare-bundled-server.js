const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');
const { bundledServerPath, bundledServerFolder, serverExecutableName } = require('../server-paths');

const extensionRoot = path.resolve(__dirname, '..');
const repoRoot = path.resolve(extensionRoot, '..', '..');
const binaryName = serverExecutableName();
const outputPath = bundledServerPath(extensionRoot);
const outputDir = path.dirname(outputPath);

const releaseDir = process.env.FOCH_LSP_RELEASE_DIR || path.join(repoRoot, 'target', 'release');
const sourcePath = path.join(releaseDir, binaryName);

const build = spawnSync(
	'cargo',
	['build', '--release', '--bin', 'foch_lsp'],
	{
		cwd: repoRoot,
		stdio: 'inherit'
	}
);

if (build.status !== 0) {
	process.exit(build.status || 1);
}

if (!fs.existsSync(sourcePath)) {
	console.error(`release binary not found: ${sourcePath}`);
	process.exit(1);
}

fs.mkdirSync(outputDir, { recursive: true });
fs.copyFileSync(sourcePath, outputPath);
if (process.platform !== 'win32') {
	fs.chmodSync(outputPath, 0o755);
}

console.log(`bundled server ready: ${outputPath}`);
console.log(`bundle key: ${bundledServerFolder()}`);
