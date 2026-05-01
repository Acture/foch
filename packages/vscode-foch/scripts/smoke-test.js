const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');
const { bundledServerArgs, bundledServerPath } = require('../server-paths');

const extensionRoot = path.resolve(__dirname, '..');
const packageJson = JSON.parse(
	fs.readFileSync(path.join(extensionRoot, 'package.json'), 'utf8')
);
const serverPath = bundledServerPath(extensionRoot);

if (!packageJson.preview) {
	console.error('package.json must mark the extension as preview');
	process.exit(1);
}

if (!fs.existsSync(serverPath)) {
	console.error(`bundled server missing: ${serverPath}`);
	process.exit(1);
}

const child = spawn(serverPath, bundledServerArgs(), {
	stdio: 'pipe'
});

let settled = false;

child.on('error', (error) => {
	if (settled) {
		return;
	}
	settled = true;
	console.error(`failed to start bundled server: ${error.message}`);
	process.exit(1);
});

setTimeout(() => {
	if (settled) {
		return;
	}
	settled = true;
	child.kill();
	console.log(`smoke test ok: ${serverPath}`);
	process.exit(0);
}, 500);
