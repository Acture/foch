const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const extensionRoot = path.resolve(__dirname, '..');
const distRoot = path.join(extensionRoot, 'dist');
const outputPath = path.join(distRoot, 'extension.js');

const build = spawnSync(
	'bun',
	[
		'build',
		'./extension.js',
		'--target=node',
		'--format=cjs',
		'--external',
		'vscode',
		'--outfile',
		'./dist/extension.js',
		'--banner',
		'globalThis.__fochExtensionDistDir = __dirname;'
	],
	{
		cwd: extensionRoot,
		stdio: 'inherit'
	}
);

if (build.error) {
	console.error(build.error.message);
	process.exit(1);
}
if (build.status !== 0) {
	process.exit(build.status || 1);
}

const languageClientRoot = path.dirname(
	require.resolve('vscode-languageclient/package.json', {
		paths: [extensionRoot]
	})
);
const terminateSource = path.join(languageClientRoot, 'lib', 'node', 'terminateProcess.sh');
const terminateDest = path.join(
	distRoot,
	'vendor',
	'vscode-languageclient',
	'lib',
	'node',
	'terminateProcess.sh'
);

fs.mkdirSync(path.dirname(terminateDest), { recursive: true });
fs.copyFileSync(terminateSource, terminateDest);
fs.chmodSync(terminateDest, 0o755);

const source = fs.readFileSync(outputPath, 'utf8');
const patched = source.replace(
	/var __dirname = ".*?vscode-languageclient\/lib\/node";/,
	'var __dirname = require("path").join(globalThis.__fochExtensionDistDir, "vendor", "vscode-languageclient", "lib", "node");'
);
if (patched === source) {
	console.error('failed to patch bundled vscode-languageclient process helper path');
	process.exit(1);
}
fs.writeFileSync(outputPath, patched);
