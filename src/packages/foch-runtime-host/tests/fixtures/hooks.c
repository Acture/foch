#include <windows.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include "foch_plugin.h"

#ifndef FIXTURE_MODE
#define FIXTURE_MODE 0
#endif

#define PROVINCES 128
#define MAX_WORKERS 8

static const FochHostApi *saved_host;
static void *const *high_next;
static void *const *low_next;
static void *const *short_next;
static FochHook *high_hook;
static unsigned char *hookable;
static volatile LONG calls;
static char *trace_buf;
static size_t trace_cap;
static char main_trace[64];
static DWORD caller_thread;
static DWORD original_thread;
static DWORD worker_ids[MAX_WORKERS];
static int worker_count;
static int province_count;
static int provinces_on_workers;
static int caller_computed;
static int joined_value;
static int serial_value;
static int original_value;
static int workers_finished;
static LONG parts_done_before_original;
static volatile LONG parts_done;
static volatile LONG post_main_entries;
static volatile LONG published_snapshot;
static volatile LONG snapshot_ready;
static volatile LONG entries_seen;
static volatile LONG snapshot_read;
static const char *redirect_at_entry;

struct province {
	int tax;
	int production;
	int autonomy;
};

struct work_part {
	int from;
	int to;
	int sum;
	DWORD tid;
};

static struct province provinces[PROVINCES];
static DWORD province_owner[PROVINCES];

static int province_income(const struct province *province) {
	return (province->tax * 12 + province->production * 8) * (100 - province->autonomy) / 100;
}

static void fill_provinces(void) {
	int i;
	for (i = 0; i < PROVINCES; ++i) {
		provinces[i].tax = 1 + (i % 12);
		provinces[i].production = 1 + (i % 9);
		provinces[i].autonomy = (i * 7) % 80;
		province_owner[i] = 0;
	}
}

static int serial_income(void) {
	int sum = 0;
	int i;
	for (i = 0; i < PROVINCES; ++i) sum += province_income(&provinces[i]);
	return sum;
}

static DWORD WINAPI worker_part(void *param) {
	struct work_part *part = param;
	int i;
	int sum = 0;
	part->tid = GetCurrentThreadId();
	for (i = part->from; i < part->to; ++i) {
		sum += province_income(&provinces[i]);
		province_owner[i] = part->tid;
	}
	part->sum = sum;
	InterlockedIncrement(&parts_done);
	return 0;
}

static DWORD WINAPI read_snapshot(void *param) {
	DWORD *tid = param;
	*tid = GetCurrentThreadId();
	if (!snapshot_ready) return 1;
	snapshot_read = published_snapshot;
	entries_seen = post_main_entries;
	return 0;
}

static void *site(const char *name) {
	typedef void *(*Fn)(void);
	Fn fn = (Fn)GetProcAddress(GetModuleHandleW(NULL), name);
	return fn ? fn() : NULL;
}

static void add_token(const char *token) {
	size_t n;
	if (!trace_buf || trace_cap == 0) return;
	n = strlen(trace_buf);
	if (n && n + 1 < trace_cap) trace_buf[n++] = ',';
	if (n >= trace_cap) return;
	snprintf(trace_buf + n, trace_cap - n, "%s", token);
}

static void write_proof(const char *name, const char *text) {
	char path[128];
	HANDLE file;
	DWORD wrote = 0;
	snprintf(path, sizeof(path), "foch-host\\%s", name);
	file = CreateFileA(path, GENERIC_WRITE, FILE_SHARE_READ, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
	if (file == INVALID_HANDLE_VALUE) return;
	WriteFile(file, text, (DWORD)strlen(text), &wrote, NULL);
	CloseHandle(file);
}

static unsigned fnv_exec(const FochHostApi *host) {
	const unsigned char *base = (const unsigned char *)host->exe_base;
	size_t image = host->exe_size;
	unsigned fnv = 2166136261u;
	int nt;
	unsigned short count, opt;
	const unsigned char *sec;
	unsigned i, n;
	if (image < 0x100 || base[0] != 'M' || base[1] != 'Z') return 0;
	memcpy(&nt, base + 60, sizeof(nt));
	if (nt < 0 || (size_t)nt + 256 > image) return 0;
	count = 0;
	opt = 0;
	memcpy(&count, base + nt + 6, sizeof(count));
	memcpy(&opt, base + nt + 20, sizeof(opt));
	sec = base + nt + 24 + opt;
	for (i = 0; i < count; ++i) {
		unsigned vsize = 0, va = 0, chars = 0;
		const unsigned char *s = sec + i * 40;
		const unsigned char *p;
		memcpy(&vsize, s + 8, sizeof(vsize));
		memcpy(&va, s + 12, sizeof(va));
		memcpy(&chars, s + 36, sizeof(chars));
		if ((chars & 0x20000000u) == 0 || vsize == 0) continue;
		if ((size_t)va + vsize > image) return 0;
		p = base + va;
		for (n = 0; n < vsize; ++n) {
			fnv ^= p[n];
			fnv *= 16777619u;
		}
	}
	return fnv;
}

static int high_detour(void) {
	LONG n = InterlockedIncrement(&calls);
	typedef int (*Fn)(void);
	Fn next;
	int value;
	if (n >= 3) {
		struct work_part parts[MAX_WORKERS];
		HANDLE threads[MAX_WORKERS];
		SYSTEM_INFO info;
		DWORD wait = WAIT_FAILED;
		int i;
		int spawned = 0;
		int cursor = 0;
		InterlockedIncrement(&post_main_entries);
		caller_thread = GetCurrentThreadId();
		redirect_at_entry = (hookable[0] == 0xFF && hookable[1] == 0x25) ? "ff25" : "other";
		fill_provinces();
		GetSystemInfo(&info);
		worker_count = (int)info.dwNumberOfProcessors;
		if (worker_count < 2) worker_count = 2;
		if (worker_count > MAX_WORKERS) worker_count = MAX_WORKERS;
		if (worker_count > PROVINCES) worker_count = PROVINCES;
		for (i = 0; i < worker_count; ++i) {
			int share = (PROVINCES - cursor) / (worker_count - i);
			parts[i].from = cursor;
			parts[i].to = cursor + share;
			parts[i].sum = 0;
			parts[i].tid = 0;
			cursor += share;
			threads[i] = CreateThread(NULL, 0, worker_part, &parts[i], 0, NULL);
			if (threads[i]) spawned += 1;
		}
		if (spawned == worker_count) {
			wait = WaitForMultipleObjects((DWORD)worker_count, threads, TRUE, 5000);
		}
		for (i = 0; i < worker_count; ++i) {
			if (threads[i]) CloseHandle(threads[i]);
		}
		workers_finished = wait == WAIT_OBJECT_0;
		parts_done_before_original = parts_done;
		joined_value = 0;
		provinces_on_workers = 0;
		caller_computed = 0;
		province_count = PROVINCES;
		for (i = 0; i < worker_count; ++i) {
			worker_ids[i] = parts[i].tid;
			joined_value += parts[i].sum;
		}
		for (i = 0; i < PROVINCES; ++i) {
			if (province_owner[i] == 0) continue;
			if (province_owner[i] == caller_thread) caller_computed += 1;
			else provinces_on_workers += 1;
		}
		serial_value = serial_income();
		main_trace[0] = 0;
		trace_buf = main_trace;
		trace_cap = sizeof(main_trace);
		add_token("high");
		next = (Fn)(*high_next);
		original_thread = GetCurrentThreadId();
		value = next();
		original_value = value;
		if (value == 7) add_token("orig");
		InterlockedExchange(&published_snapshot, joined_value);
		InterlockedExchange(&snapshot_ready, 1);
		return value;
	}
	add_token("high");
	next = (Fn)(*high_next);
	return next();
}

static int low_detour(void) {
	typedef int (*Fn)(void);
	Fn next;
	add_token("low");
	next = (Fn)(*low_next);
	return next();
}

static int entry_init(const FochHostApi *host) {
	void *unique_found = NULL;
	void *missing_found = NULL;
	void *ambiguous_found = NULL;
	void *expected;
	int missing, ambiguous, unfit, chain_rc, low_rc;
	unsigned before_hash, after_hash;
	unsigned char prologue[16], short_before[6], short_after[6];
	char first[64], second[64], proof[1024];
	int first_value, second_value;
	FochHook *low_hook = NULL;
	FochHook *short_hook = NULL;
	FochStatus active = { sizeof(FochStatus), FOCH_STATE_ACTIVE, 0, { "hooks init", 10 } };
	FochStr config = host->config_json(host);
	saved_host = host;
	if (host->abi_major != FOCH_ABI_MAJOR || !host->exe_base || !host->exe_size) return FOCH_E_INVALID;
	if (config.len != 17 || memcmp(config.ptr, "{\"greeting\":\"hi\"}", 17) != 0) return FOCH_E_INVALID;

	before_hash = fnv_exec(host);
	missing = host->find_pattern(host, (FochStr){ "F1 C4 11 22 33 44 55 66 77 88 99 AA BB CD", sizeof("F1 C4 11 22 33 44 55 66 77 88 99 AA BB CD") - 1 }, &missing_found);
	ambiguous = host->find_pattern(host, (FochStr){ "DE AD BE EF", sizeof("DE AD BE EF") - 1 }, &ambiguous_found);
	unique_found = NULL;
	host->find_pattern(host, (FochStr){ "F0 C4 11 22 33 44 55 66 77 88 99 AA BB CC", sizeof("F0 C4 11 22 33 44 55 66 77 88 99 AA BB CC") - 1 }, &unique_found);
	after_hash = fnv_exec(host);
	expected = site("fixture_unique_site");

	hookable = site("fixture_hookable_site");
	memcpy(prologue, hookable, sizeof(prologue));
	chain_rc = host->hook_create(host, hookable, (void *)high_detour, 20, &high_next, &high_hook);
	low_rc = host->hook_create(host, hookable, (void *)low_detour, 10, &low_next, &low_hook);
	host->hook_set_enabled(host, high_hook, 1);
	host->hook_set_enabled(host, low_hook, 1);
	first[0] = 0;
	trace_buf = first;
	trace_cap = sizeof(first);
	first_value = high_detour();
	if (first_value == 7) add_token("orig");
	host->hook_set_enabled(host, low_hook, 0);
	second[0] = 0;
	trace_buf = second;
	trace_cap = sizeof(second);
	second_value = high_detour();
	if (second_value == 7) add_token("orig");

	memcpy(short_before, site("fixture_short_site"), sizeof(short_before));
	unfit = host->hook_create(host, site("fixture_short_site"), (void *)low_detour, 1, &short_next, &short_hook);
	if (unfit == FOCH_OK) unfit = host->hook_set_enabled(host, short_hook, 1);
	memcpy(short_after, site("fixture_short_site"), sizeof(short_after));

	snprintf(proof, sizeof(proof),
		"unique=%p\nexpected=%p\nmissing=%d\nambiguous=%d\nlookup-bytes=%s\n"
		"chain=%s\nchain-after-disable=%s\nchain-bytes=%s\nchain-create=%d\nchain-low=%d\n"
		"unfit=%d\nunfit-bytes=%s\n",
		unique_found, expected, missing, ambiguous,
		before_hash == after_hash && before_hash != 0 ? "unchanged" : "changed",
		first, second,
		memcmp(prologue, hookable, sizeof(prologue)) == 0 ? "unchanged" : "changed",
		chain_rc, low_rc, unfit,
		memcmp(short_before, short_after, sizeof(short_before)) == 0 ? "unchanged" : "changed");
	write_proof("hook-entry.txt", proof);
	(void)missing_found;
	(void)ambiguous_found;
	host->report(host, &active);
	return FOCH_OK;
}

static int late_init(const FochHostApi *host) {
	unsigned char before[16], after[16];
	void *const *next = NULL;
	FochHook *hook = NULL;
	unsigned char *late;
	int created, enabled;
	char proof[256];
	FochStatus active = { sizeof(FochStatus), FOCH_STATE_ACTIVE, 0, { "late init", 9 } };
	FochStr config = host->config_json(host);
	if (host->abi_major != FOCH_ABI_MAJOR || !host->exe_base || !host->exe_size) return FOCH_E_INVALID;
	if (config.len != 17 || memcmp(config.ptr, "{\"greeting\":\"hi\"}", 17) != 0) return FOCH_E_INVALID;
	late = site("fixture_late_site");
	memcpy(before, late, sizeof(before));
	created = host->hook_create(host, late, (void *)high_detour, 1, &next, &hook);
	enabled = host->hook_set_enabled(host, hook, 1);
	memcpy(after, late, sizeof(after));
	snprintf(proof, sizeof(proof), "deferred-create=%d\ndeferred-enable=%d\ndeferred-bytes=%s\n",
		created, enabled, memcmp(before, after, sizeof(before)) == 0 ? "unchanged" : "changed");
	write_proof("hook-late.txt", proof);
	host->report(host, &active);
	return FOCH_OK;
}

/* Called by the fixture after the intercepted call has returned. The reader
   is a different thread and does not enter the hooked function. Disabling
   the detour happens here, after that return, so the call path itself does
   not take the host hook mutex. */
__declspec(dllexport) int fixture_observe_flow(void) {
	DWORD reader_tid = 0;
	HANDLE reader;
	LONG entries_before;
	LONG entries_after;
	const char *after_call;
	const char *after_disable;
	char proof[1280];
	if (!snapshot_ready || !saved_host || !hookable) return 1;
	entries_before = post_main_entries;
	reader = CreateThread(NULL, 0, read_snapshot, &reader_tid, 0, NULL);
	if (!reader) return 2;
	if (WaitForSingleObject(reader, 5000) != WAIT_OBJECT_0) {
		CloseHandle(reader);
		return 3;
	}
	CloseHandle(reader);
	entries_after = post_main_entries;
	after_call = (hookable[0] == 0xFF && hookable[1] == 0x25) ? "ff25" : "other";
	saved_host->hook_set_enabled(saved_host, high_hook, 0);
	after_disable = (hookable[0] == 0xFF && hookable[1] == 0x25) ? "ff25" : "other";
	snprintf(proof, sizeof(proof),
		"redirect-call=%s\nredirect-bytes=%s\nredirect-after-disable=%s\nredirect-value=%d\n"
		"caller-thread=%lu\nworker-count=%d\nworker-1=%lu\nworker-2=%lu\n"
		"province-count=%d\nprovinces-on-workers=%d\ncaller-computed=%d\n"
		"joined=%d\nserial=%d\noriginal-after-join=%d\nworkers-before-original=%ld\noriginal-thread=%lu\n"
		"snapshot=%ld\nsnapshot-thread=%lu\nentries-before-snapshot=%ld\npost-main-entries=%ld\nentries-after-snapshot=%ld\n"
		"prologue-after-call=%s\n",
		main_trace, redirect_at_entry ? redirect_at_entry : "missing", after_disable, original_value,
		caller_thread, worker_count, worker_ids[0], worker_ids[1],
		province_count, provinces_on_workers, caller_computed,
		joined_value, serial_value, workers_finished, parts_done_before_original, original_thread,
		snapshot_read, reader_tid, entries_before, entries_seen, entries_after,
		after_call);
	write_proof("hook-main.txt", proof);
	return 0;
}

static void status(FochStatus *out) {
	*out = (FochStatus){ sizeof(FochStatus), FOCH_STATE_ACTIVE, 0, { "hooks status", 12 } };
}

static const FochPluginApi api = {
	sizeof(FochPluginApi), FOCH_ABI_MAJOR, FOCH_ABI_MINOR, 0,
	FIXTURE_MODE == 1 ? late_init : entry_init,
	status, NULL, NULL, NULL
};

__declspec(dllexport) const FochPluginApi *foch_plugin_get_api(uint32_t major) {
	(void)major;
	return &api;
}
