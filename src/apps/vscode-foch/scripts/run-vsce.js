const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');
const { bundledServerPath, vsceTarget } = require('../server-paths');

const extensionRoot = path.resolve(__dirname, '..');
const mode = process.argv[2];
const extraArgs = process.argv.slice(3);

if (!mode || !['package', 'publish'].includes(mode)) {
	console.error('usage: node ./scripts/run-vsce.js <package|publish> [args...]');
	process.exit(2);
}

const bundledServer = bundledServerPath(extensionRoot);
if (!fs.existsSync(bundledServer)) {
	console.error(`missing bundled server: ${bundledServer}`);
	console.error('run `npm run prepare:server` first');
	process.exit(1);
}

const target = vsceTarget();
// Run the locked vsce's own entry script with this node. The package manager's
// .bin shims differ per platform (bun writes no vsce.cmd on Windows), and npx is
// npx.cmd there, which spawnSync cannot start without a shell.
const vscePackageJson = require.resolve('@vscode/vsce/package.json', {
	paths: [extensionRoot]
});
const vsceScript = path.join(
	path.dirname(vscePackageJson),
	require(vscePackageJson).bin.vsce
);
const args = [
	vsceScript,
	mode,
	'--pre-release',
	'--target',
	target,
	'--no-dependencies',
	...extraArgs
];

console.log(`running ${mode} for target ${target}`);
const result = spawnSync(process.execPath, args, {
	cwd: extensionRoot,
	stdio: 'inherit',
	env: process.env
});

if (result.error) {
	console.error(result.error.message);
	process.exit(1);
}

process.exit(result.status ?? 1);
