const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');
const { bundledServerArgs, bundledServerPath } = require('../server-paths');

const extensionRoot = path.resolve(__dirname, '..');
const packageJson = JSON.parse(
	fs.readFileSync(path.join(extensionRoot, 'package.json'), 'utf8')
);
const serverPath = bundledServerPath(extensionRoot);
const timeoutMs = 5000;
let child = undefined;
let stderr = '';
let stdoutBuffer = Buffer.alloc(0);
let nextRequestId = 1;
const pending = new Map();

function fail(message) {
	if (child && !child.killed) {
		child.kill();
	}
	const suffix = stderr.trim() ? `\nserver stderr:\n${stderr.trim()}` : '';
	console.error(`${message}${suffix}`);
	process.exit(1);
}

function assert(condition, message) {
	if (!condition) {
		fail(message);
	}
}

if (!packageJson.preview) {
	fail('package.json must mark the extension as preview');
}

if (packageJson.version !== '0.1.0') {
	fail(`package.json version must be 0.1.0 for this preview, got ${packageJson.version}`);
}

const languages = packageJson.contributes && packageJson.contributes.languages
	? packageJson.contributes.languages
	: [];
assert(
	languages.some((language) => language.id === 'foch-eu4'),
	'package.json must contribute the foch-eu4 language id'
);

const configProperties =
	packageJson.contributes &&
	packageJson.contributes.configuration &&
	packageJson.contributes.configuration.properties
		? packageJson.contributes.configuration.properties
		: {};
for (const key of [
	'fochLsp.serverPath',
	'fochLsp.serverArgs',
	'fochLsp.serverCwd',
	'fochLsp.gamePath',
	'fochLsp.modPaths',
	'fochLsp.autoDetectMods',
	'fochLsp.autoDetectModsMax'
]) {
	assert(configProperties[key], `package.json missing configuration key: ${key}`);
}

const mainPath = path.resolve(extensionRoot, packageJson.main || '');
if (!fs.existsSync(mainPath)) {
	fail(`bundled extension entry missing: ${mainPath}`);
}

const extensionEntry = fs.readFileSync(mainPath, 'utf8');
assert(
	extensionEntry.includes('foch.createLocalisationStub'),
	'extension bundle must register the missing-localisation quick fix command'
);
assert(
	extensionEntry.includes('Foch is inactive in this workspace'),
	'extension bundle must avoid starting the LSP in unrelated workspaces'
);
assert(
	extensionEntry.includes('Foch LSP settings changed') &&
		extensionEntry.includes('workbench.action.reloadWindow'),
	'extension bundle must prompt for window reload after LSP settings change'
);

const languageClientHelper = path.join(
	extensionRoot,
	'dist',
	'vendor',
	'vscode-languageclient',
	'lib',
	'node',
	'terminateProcess.sh'
);
if (!fs.existsSync(languageClientHelper)) {
	fail(`bundled vscode-languageclient helper missing: ${languageClientHelper}`);
}

if (!fs.existsSync(serverPath)) {
	fail(`bundled server missing: ${serverPath}`);
}

function writeMessage(message) {
	const body = JSON.stringify(message);
	child.stdin.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\r\n\r\n${body}`);
}

function request(method, params) {
	const id = nextRequestId++;
	const message = {
		jsonrpc: '2.0',
		id,
		method
	};
	if (params !== undefined) {
		message.params = params;
	}
	return new Promise((resolve, reject) => {
		pending.set(id, { resolve, reject });
		writeMessage(message);
	});
}

function notify(method, params) {
	writeMessage({
		jsonrpc: '2.0',
		method,
		params
	});
}

function handleMessage(message) {
	if (message.id === undefined) {
		return;
	}
	const waiter = pending.get(message.id);
	if (!waiter) {
		return;
	}
	pending.delete(message.id);
	if (message.error) {
		waiter.reject(new Error(`${message.error.code}: ${message.error.message}`));
	} else {
		waiter.resolve(message.result);
	}
}

function consumeMessages(chunk) {
	stdoutBuffer = Buffer.concat([stdoutBuffer, chunk]);
	while (true) {
		const headerEnd = stdoutBuffer.indexOf('\r\n\r\n');
		if (headerEnd < 0) {
			return;
		}
		const header = stdoutBuffer.subarray(0, headerEnd).toString('ascii');
		const lengthMatch = /^Content-Length:\s*(\d+)/im.exec(header);
		if (!lengthMatch) {
			fail(`invalid LSP response header: ${header}`);
		}
		const bodyLength = Number(lengthMatch[1]);
		const bodyStart = headerEnd + 4;
		const bodyEnd = bodyStart + bodyLength;
		if (stdoutBuffer.length < bodyEnd) {
			return;
		}
		const body = stdoutBuffer.subarray(bodyStart, bodyEnd).toString('utf8');
		stdoutBuffer = stdoutBuffer.subarray(bodyEnd);
		handleMessage(JSON.parse(body));
	}
}

function waitForExit() {
	return new Promise((resolve) => {
		const fallback = setTimeout(() => {
			if (!child.killed) {
				child.kill();
			}
			resolve();
		}, 1000);
		child.once('exit', () => {
			clearTimeout(fallback);
			resolve();
		});
	});
}

function assertServerCapabilities(result) {
	const capabilities = result && result.capabilities ? result.capabilities : {};
	assert(capabilities.textDocumentSync !== undefined, 'LSP initialize missing textDocumentSync');
	assert(capabilities.completionProvider, 'LSP initialize missing completionProvider');
	assert(capabilities.hoverProvider, 'LSP initialize missing hoverProvider');
	assert(capabilities.definitionProvider, 'LSP initialize missing definitionProvider');
	assert(capabilities.referencesProvider, 'LSP initialize missing referencesProvider');
	assert(capabilities.documentSymbolProvider, 'LSP initialize missing documentSymbolProvider');
	assert(capabilities.workspaceSymbolProvider, 'LSP initialize missing workspaceSymbolProvider');
	assert(capabilities.codeActionProvider, 'LSP initialize missing codeActionProvider');

	const codeActionKinds = capabilities.codeActionProvider.codeActionKinds || [];
	assert(
		codeActionKinds.includes('quickfix'),
		'LSP initialize must advertise quickfix code actions'
	);
}

async function main() {
	child = spawn(serverPath, bundledServerArgs(), {
		stdio: 'pipe'
	});

	child.stdout.on('data', consumeMessages);
	child.stderr.on('data', (chunk) => {
		stderr += chunk.toString('utf8');
	});

	child.on('error', (error) => {
		fail(`failed to start bundled server: ${error.message}`);
	});

	const timeout = setTimeout(() => {
		fail(`LSP smoke test timed out after ${timeoutMs}ms`);
	}, timeoutMs);

	const initializeResult = await request('initialize', {
		processId: process.pid,
		clientInfo: {
			name: 'foch-vscode-smoke',
			version: packageJson.version
		},
		rootUri: null,
		capabilities: {},
		workspaceFolders: null
	});
	assertServerCapabilities(initializeResult);
	notify('initialized', {});
	await request('shutdown');
	notify('exit', {});
	await waitForExit();
	clearTimeout(timeout);
	console.log(`smoke test ok: ${serverPath}`);
}

main().catch((error) => {
	fail(error && error.message ? error.message : String(error));
});
