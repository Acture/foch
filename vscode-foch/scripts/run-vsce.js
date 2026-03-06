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
const localVsce = path.join(
	extensionRoot,
	'node_modules',
	'.bin',
	process.platform === 'win32' ? 'vsce.cmd' : 'vsce'
);
const hasLocalVsce = fs.existsSync(localVsce);
const command = hasLocalVsce ? localVsce : 'npx';
const args = hasLocalVsce
	? [mode, '--pre-release', '--target', target, ...extraArgs]
	: ['@vscode/vsce', mode, '--pre-release', '--target', target, ...extraArgs];

console.log(`running ${mode} for target ${target}`);
const result = spawnSync(command, args, {
	cwd: extensionRoot,
	stdio: 'inherit',
	env: process.env
});

if (result.error) {
	console.error(result.error.message);
	process.exit(1);
}

process.exit(result.status ?? 1);
