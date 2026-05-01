const path = require('path');
const fs = require('fs');
const vscode = require('vscode');
const { LanguageClient, TransportKind } = require('vscode-languageclient/node');
const { bundledServerArgs, bundledServerPath } = require('./server-paths');

let client;

function normalizePath(p) {
	if (!p || typeof p !== 'string') {
		return '';
	}
	return p.trim();
}

function existingDir(p) {
	if (!p) {
		return false;
	}
	try {
		return fs.statSync(p).isDirectory();
	} catch (_) {
		return false;
	}
}

function existingFile(p) {
	if (!p) {
		return false;
	}
	try {
		return fs.statSync(p).isFile();
	} catch (_) {
		return false;
	}
}

function buildDocumentSelector() {
	return [
		{ scheme: 'file', pattern: '**/events/**/*.{txt,lua}' },
		{ scheme: 'file', pattern: '**/decisions/**/*.{txt,lua}' },
		{ scheme: 'file', pattern: '**/common/scripted_effects/**/*.{txt,lua}' },
		{ scheme: 'file', pattern: '**/common/diplomatic_actions/**/*.{txt,lua}' },
		{ scheme: 'file', pattern: '**/common/triggered_modifiers/**/*.{txt,lua}' },
		{ scheme: 'file', pattern: '**/common/defines/**/*.{txt,lua}' },
		{ scheme: 'file', pattern: '**/interface/**/*.{txt,gui}' },
		{ scheme: 'file', pattern: '**/common/interface/**/*.{txt,gui}' },
		{ scheme: 'file', pattern: '**/gfx/**/*.gfx' }
	];
}

function normalizeFsPath(fsPath) {
	return normalizePath(fsPath).replace(/\\/g, '/');
}

function isEu4ScriptPath(fsPath) {
	const p = normalizeFsPath(fsPath).toLowerCase();
	return (
		p.includes('/events/') ||
		p.includes('/decisions/') ||
		p.includes('/common/scripted_effects/') ||
		p.includes('/common/diplomatic_actions/') ||
		p.includes('/common/triggered_modifiers/') ||
		p.includes('/common/defines/') ||
		p.includes('/interface/') ||
		p.includes('/common/interface/') ||
		p.includes('/gfx/')
	);
}

async function detectModRootsFromDescriptor(maxResults) {
	const uris = await vscode.workspace.findFiles(
		'**/descriptor.mod',
		'**/{.git,node_modules,target,.vscode,.idea}/**',
		maxResults
	);
	const out = [];
	for (const uri of uris) {
		const root = path.dirname(uri.fsPath);
		if (existingDir(root)) {
			out.push({ path: root, role: 'mod' });
		}
	}
	return out;
}

function dedupTargets(targets) {
	const dedup = new Map();
	for (const item of targets) {
		const key = `${item.role}::${normalizeFsPath(item.path)}`;
		if (!dedup.has(key)) {
			dedup.set(key, item);
		}
	}
	return Array.from(dedup.values());
}

async function buildTargets(cfg) {
	let targets = buildConfiguredTargets(cfg);
	const autoDetectMods = cfg.get('autoDetectMods', true);
	const autoDetectMax = Number(cfg.get('autoDetectModsMax', 300)) || 300;
	if (autoDetectMods) {
		try {
			const detected = await detectModRootsFromDescriptor(autoDetectMax);
			targets = targets.concat(detected);
		} catch (_) {
			// keep startup resilient; falling back to configured/workspace targets
		}
	}

	if (targets.length === 0 && vscode.workspace.workspaceFolders) {
		for (const folder of vscode.workspace.workspaceFolders) {
			targets.push({ path: folder.uri.fsPath, role: 'mod' });
		}
	}

	return dedupTargets(targets);
}

function buildConfiguredTargets(cfg) {
	const targets = [];
	const gamePath = normalizePath(cfg.get('gamePath'));
	if (existingDir(gamePath)) {
		targets.push({ path: gamePath, role: 'game' });
	}

	const modPaths = cfg.get('modPaths') || [];
	for (const raw of modPaths) {
		const modPath = normalizePath(raw);
		if (existingDir(modPath)) {
			targets.push({ path: modPath, role: 'mod' });
		}
	}
	return targets;
}

async function maybeSetEu4Language(document) {
	if (!document || document.uri.scheme !== 'file') {
		return;
	}
	if (!isEu4ScriptPath(document.uri.fsPath)) {
		return;
	}
	if (document.languageId === 'foch-eu4') {
		return;
	}
	try {
		await vscode.languages.setTextDocumentLanguage(document, 'foch-eu4');
	} catch (_) {
		// ignore; some virtual/readonly docs may reject language changes
	}
}

function normalizeServerInvocation(serverPath, serverArgs) {
	const args = Array.isArray(serverArgs) ? [...serverArgs] : [];
	const cmd = (serverPath || '').trim();
	const base = path.basename(cmd).toLowerCase();
	const isCargo = cmd === 'cargo' || base === 'cargo' || base === 'cargo.exe';
	const isCargoRun = args.length > 0 && args[0] === 'run';
	if (isCargo && isCargoRun && !args.includes('--')) {
		// vscode-languageclient appends "--stdio"; for cargo run we must split cargo args and bin args.
		args.push('--');
	}
	return args;
}

function resolveServerCommand(cfg, extensionPath) {
	const configuredPath = normalizePath(cfg.get('serverPath'));
	if (configuredPath) {
		const configuredArgs = cfg.get('serverArgs') || [];
		return {
			command: configuredPath,
			args: normalizeServerInvocation(configuredPath, configuredArgs),
			mode: 'configured'
		};
	}

	const bundledPath = bundledServerPath(extensionPath);
	if (existingFile(bundledPath)) {
		return {
			command: bundledPath,
			args: bundledServerArgs(),
			mode: 'bundled'
		};
	}

	const cargoArgs = ['run', '--quiet', '--bin', 'foch', '--', 'lsp'];
	return {
		command: 'cargo',
		args: normalizeServerInvocation('cargo', cargoArgs),
		mode: 'cargo-fallback'
	};
}

async function activate(context) {
	const cfg = vscode.workspace.getConfiguration('fochLsp');
	const configuredCwd = normalizePath(cfg.get('serverCwd'));
	const workspaceCwd = vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders.length > 0
		? vscode.workspace.workspaceFolders[0].uri.fsPath
		: process.cwd();
	const cwd = configuredCwd || workspaceCwd;
	const server = resolveServerCommand(cfg, context.extensionPath);

	const targets = await buildTargets(cfg);
	const env = { ...process.env };
	if (targets.length > 0) {
		env.FOCH_LSP_TARGETS_JSON = JSON.stringify(targets);
	}

	const serverOptions = {
		run: {
			command: server.command,
			args: server.args,
			transport: TransportKind.stdio,
			options: { cwd, env }
		},
		debug: {
			command: server.command,
			args: server.args,
			transport: TransportKind.stdio,
			options: { cwd, env }
		}
	};

	const clientOptions = {
		documentSelector: buildDocumentSelector(),
		outputChannelName: 'Foch'
	};

	client = new LanguageClient(
		'foch',
		'Foch',
		serverOptions,
		clientOptions
	);

	context.subscriptions.push(client.start());

	context.subscriptions.push(
		vscode.workspace.onDidOpenTextDocument((doc) => {
			void maybeSetEu4Language(doc);
		})
	);
	context.subscriptions.push(
		vscode.window.onDidChangeVisibleTextEditors((editors) => {
			for (const editor of editors) {
				void maybeSetEu4Language(editor.document);
			}
		})
	);
	for (const doc of vscode.workspace.textDocuments) {
		await maybeSetEu4Language(doc);
	}
	for (const editor of vscode.window.visibleTextEditors) {
		await maybeSetEu4Language(editor.document);
	}
	if (server.mode === 'cargo-fallback') {
		void vscode.window.showWarningMessage(
			'Foch is using cargo fallback. Bundle the foch binary before publishing the extension.'
		);
	}
}

function deactivate() {
	if (!client) {
		return undefined;
	}
	return client.stop();
}

module.exports = {
	activate,
	deactivate
};
